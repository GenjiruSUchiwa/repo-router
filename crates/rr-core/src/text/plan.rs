//! The deterministic, budgeted page planner.
//!
//! A directory whose records do not fit in one page is split into overflow
//! pages, and if the links to those pages do not fit either, into a level of
//! pages that link to pages. Nothing is ever truncated, elided, or summarized
//! to make something fit — the budget decides how many files there are, never
//! what they say.
//!
//! Every size used here comes from the renderer that will actually write the
//! bytes ([`super::render`]), not from a second estimate that could drift from
//! it. A test renders each planned page and asserts the two agree.

use std::ops::Range;

use super::digest::HashStream;
use super::model::{ApiRecord, ChildRecord, ScopePath, TestRecord};
use super::render;

/// The three record lists of one scope, in the order they are packed.
///
/// Bundled into one type because the planner, the sizer, and the renderer all
/// need the same three slices, and three parameters repeated across a dozen
/// signatures is where an argument eventually gets passed in the wrong order.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Records<'a> {
    pub(crate) children: &'a [ChildRecord],
    pub(crate) api: &'a [ApiRecord],
    pub(crate) tests: &'a [TestRecord],
}

/// Which list a flattened record index refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Child(usize),
    Api(usize),
    Test(usize),
}

impl Records<'_> {
    pub(crate) fn len(&self) -> usize {
        self.children.len() + self.api.len() + self.tests.len()
    }

    /// Resolves a flattened index into the list that holds it.
    ///
    /// The flattened order is `Children`, `API`, `Tests` — the same order the
    /// sections appear in a rendered page, so a page is always a contiguous
    /// run and never has to remember which records it skipped.
    pub(crate) fn at(&self, index: usize) -> Slot {
        if index < self.children.len() {
            return Slot::Child(index);
        }
        let index = index - self.children.len();
        if index < self.api.len() {
            return Slot::Api(index);
        }
        Slot::Test(index - self.api.len())
    }

    /// Splits a flattened range into one range per section.
    pub(crate) fn split(&self, range: &Range<usize>) -> (Range<usize>, Range<usize>, Range<usize>) {
        let children_end = self.children.len();
        let api_end = children_end + self.api.len();
        let clamp = |value: usize, low: usize, high: usize| value.clamp(low, high);
        let children = clamp(range.start, 0, children_end)..clamp(range.end, 0, children_end);
        let api = clamp(range.start, children_end, api_end) - children_end
            ..clamp(range.end, children_end, api_end) - children_end;
        let tests = clamp(range.start, api_end, self.len()) - api_end
            ..clamp(range.end, api_end, self.len()) - api_end;
        (children, api, tests)
    }
}

/// The fixed-width name of one generated overflow page.
///
/// Fixed width is what lets the planner size a link before it knows how many
/// pages there will be. Level and ordinal are stored rather than the formatted
/// string so that the name has exactly one spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PageName {
    pub(crate) level: u32,
    pub(crate) ordinal: u32,
}

impl PageName {
    /// The file name inside the scope directory.
    pub(crate) fn file_name(self) -> String {
        format!(
            "{}{:08}-{:08}{}",
            super::OVERFLOW_PREFIX,
            self.level,
            self.ordinal,
            super::MARKDOWN_EXTENSION
        )
    }
}

/// What one page holds.
///
/// A page holds a contiguous run of leaf records, or links to pages one level
/// beneath it, or — only in the router — a prefix of records followed by the
/// links to the pages that hold the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PageContent {
    pub(crate) records: Range<usize>,
    pub(crate) links: Vec<PageName>,
}

/// One planned overflow page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedPage {
    pub(crate) name: PageName,
    pub(crate) content: PageContent,
}

/// The frozen page plan of one directory scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopePlan {
    router: PageContent,
    /// Overflow pages in publication order: lowest level first, so a page is
    /// always written before whatever links to it.
    pages: Vec<PlannedPage>,
    over_budget: bool,
}

impl ScopePlan {
    /// Plans one scope against a body-byte budget.
    ///
    /// Never fails. A record too large to fit anywhere gets its own page and
    /// sets [`Self::is_over_budget`]; refusing instead would make one oversize
    /// signature block the whole repository's map from ever being written.
    pub(crate) fn build(scope: &ScopePath, records: Records<'_>, byte_budget: usize) -> Self {
        let router_capacity = byte_budget.saturating_sub(render::router_overhead(scope));
        let page_capacity = byte_budget.saturating_sub(render::page_overhead(scope));

        if let Some(all) = fits_whole(records, router_capacity) {
            return Self {
                router: PageContent {
                    records: all,
                    links: Vec::new(),
                },
                pages: Vec::new(),
                over_budget: false,
            };
        }

        match Self::router_keeping_a_prefix(records, router_capacity, page_capacity) {
            Some(plan) => plan,
            None => Self::router_of_pure_navigation(records, router_capacity, page_capacity),
        }
    }

    /// True when some page holds one record that alone exceeds the budget.
    pub(crate) const fn is_over_budget(&self) -> bool {
        self.over_budget
    }

    pub(crate) const fn router(&self) -> &PageContent {
        &self.router
    }

    pub(crate) fn pages(&self) -> &[PlannedPage] {
        &self.pages
    }

    /// The plan's contribution to `index_hash`.
    ///
    /// The plan is hashed because it decides which files exist and what is in
    /// them: two projections that pack the same records differently are not
    /// interchangeable, and a reader that fetched page three of the old plan
    /// must be able to tell.
    pub(crate) fn write_index_hash(&self, stream: &mut HashStream) {
        write_content_hash(&self.router, stream);
        stream.count(self.pages.len());
        for page in &self.pages {
            stream.u32(page.name.level);
            stream.u32(page.name.ordinal);
            write_content_hash(&page.content, stream);
        }
    }

    /// The router keeps the longest prefix of records it can, then links to the
    /// level-0 pages holding the rest.
    ///
    /// How much the router can keep depends on how many links it must carry,
    /// which depends on how much it kept. The loop settles that by assuming the
    /// page count it discovered last time. Each attempt that does not settle
    /// discovers strictly more pages than it assumed, so the assumption climbs
    /// and the loop runs at most once per record — in practice twice.
    ///
    /// Returns `None` when even a router holding no records cannot carry its
    /// own navigation, which is where the pure-navigation levels take over.
    fn router_keeping_a_prefix(
        records: Records<'_>,
        router_capacity: usize,
        page_capacity: usize,
    ) -> Option<Self> {
        let mut assumed_pages = 0;
        loop {
            let kept = greedy_prefix(records, 0, router_capacity, assumed_pages);
            let (pages, over_budget) = pack(records, kept..records.len(), page_capacity, 0);
            if pages.len() <= assumed_pages {
                // The assumption held, so the prefix the router kept was sized
                // against a link list at least as large as the real one.
                let mut sizer = render::BodySizer::new(records);
                for index in 0..kept {
                    sizer.push(index);
                }
                if sizer.bytes_with_links(pages.len()) > router_capacity {
                    return None;
                }
                return Some(Self {
                    router: PageContent {
                        records: 0..kept,
                        links: pages.iter().map(|page| page.name).collect(),
                    },
                    pages,
                    over_budget,
                });
            }
            assumed_pages = pages.len();
            if render::link_list_bytes(assumed_pages) > router_capacity {
                return None;
            }
        }
    }

    /// The router holds nothing but links, adding levels until they fit.
    ///
    /// Each level packs the previous level's links into pages and links to
    /// those instead. A level that does not reduce the count cannot help, and
    /// stopping there leaves one over-budget router — visible in
    /// [`super::TextValidation`] — rather than an unbounded tower of pages.
    fn router_of_pure_navigation(
        records: Records<'_>,
        router_capacity: usize,
        page_capacity: usize,
    ) -> Self {
        let (mut pages, mut over_budget) = pack(records, 0..records.len(), page_capacity, 0);
        let mut all = pages.clone();
        let mut level = 0_u32;
        loop {
            let names: Vec<PageName> = pages.iter().map(|page| page.name).collect();
            if render::link_list_bytes(names.len()) <= router_capacity {
                return Self {
                    router: PageContent {
                        records: 0..0,
                        links: names,
                    },
                    pages: all,
                    over_budget,
                };
            }
            level += 1;
            let grouped = pack_links(&names, page_capacity, level);
            if grouped.len() >= pages.len() {
                // Another level would not shrink anything. Publish what exists
                // and let the report say the router is over budget.
                over_budget = true;
                return Self {
                    router: PageContent {
                        records: 0..0,
                        links: names,
                    },
                    pages: all,
                    over_budget,
                };
            }
            all.extend(grouped.iter().cloned());
            pages = grouped;
        }
    }
}

fn write_content_hash(content: &PageContent, stream: &mut HashStream) {
    stream.count(content.records.start);
    stream.count(content.records.end);
    stream.count(content.links.len());
    for link in &content.links {
        stream.u32(link.level);
        stream.u32(link.ordinal);
    }
}

/// The whole record range when it fits in one page, otherwise `None`.
fn fits_whole(records: Records<'_>, capacity: usize) -> Option<Range<usize>> {
    let total = records.len();
    (greedy_prefix(records, 0, capacity, 0) == total).then_some(0..total)
}

/// The longest run starting at `start` that fits, leaving room for `links`.
///
/// The sizer starts empty even when `start` is not zero, because each page is
/// measured on its own: a page that begins mid-file pays for that file's
/// heading again, and that is exactly what the sizer reports.
fn greedy_prefix(records: Records<'_>, start: usize, capacity: usize, links: usize) -> usize {
    let mut sizer = render::BodySizer::new(records);
    let mut index = start;
    while index < records.len() && sizer.would_fit(index, capacity, links) {
        sizer.push(index);
        index += 1;
    }
    index
}

/// Packs a record range into as few pages as greedy order allows.
///
/// Returns the pages and whether any of them holds a single record that does
/// not fit on its own. Such a record is placed alone rather than dropped: an
/// omitted signature is a wrong map, while an over-budget page is a true one
/// that costs more to read.
fn pack(
    records: Records<'_>,
    range: Range<usize>,
    capacity: usize,
    level: u32,
) -> (Vec<PlannedPage>, bool) {
    let mut pages = Vec::new();
    let mut over_budget = false;
    let mut cursor = range.start;
    while cursor < range.end {
        let mut end = greedy_prefix(records, cursor, capacity, 0).min(range.end);
        if end == cursor {
            // Indivisible: one record wider than a whole page.
            end = cursor + 1;
            over_budget = true;
        }
        pages.push(PlannedPage {
            name: PageName {
                level,
                ordinal: ordinal_of(pages.len()),
            },
            content: PageContent {
                records: cursor..end,
                links: Vec::new(),
            },
        });
        cursor = end;
    }
    (pages, over_budget)
}

/// Packs page links into pages of links one level up.
fn pack_links(names: &[PageName], capacity: usize, level: u32) -> Vec<PlannedPage> {
    let mut pages = Vec::new();
    let mut cursor = 0;
    while cursor < names.len() {
        let mut taken = 0;
        while cursor + taken < names.len() && render::link_list_bytes(taken + 1) <= capacity {
            taken += 1;
        }
        // One link that does not fit still has to go somewhere; the caller's
        // no-reduction check turns that into an over-budget report.
        let taken = taken.max(1);
        pages.push(PlannedPage {
            name: PageName {
                level,
                ordinal: ordinal_of(pages.len()),
            },
            content: PageContent {
                records: 0..0,
                links: names[cursor..cursor + taken].to_vec(),
            },
        });
        cursor += taken;
    }
    pages
}

/// Ordinals are eight decimal digits, so this is where a repository with more
/// than a hundred million pages in one directory would stop being nameable.
/// Saturating keeps the name unique-by-position rather than wrapping to a name
/// another page already has.
fn ordinal_of(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX).min(99_999_999)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(name: &str) -> ChildRecord {
        ChildRecord {
            name: name.to_owned(),
            path: name.to_owned(),
        }
    }

    fn records(children: &[ChildRecord]) -> Records<'_> {
        Records {
            children,
            api: &[],
            tests: &[],
        }
    }

    #[test]
    fn a_page_name_is_fixed_width() {
        assert_eq!(
            PageName {
                level: 0,
                ordinal: 7
            }
            .file_name(),
            "MAP.rr-00000000-00000007.md"
        );
        assert_eq!(
            PageName {
                level: 0,
                ordinal: 0
            }
            .file_name()
            .len(),
            PageName {
                level: 12,
                ordinal: 99_999_999
            }
            .file_name()
            .len()
        );
    }

    #[test]
    fn a_flattened_range_splits_into_its_three_sections() {
        let children = [child("a"), child("b")];
        let all = records(&children);
        assert_eq!(all.at(0), Slot::Child(0));
        assert_eq!(all.at(1), Slot::Child(1));
        let (children_range, api, tests) = all.split(&(1..2));
        assert_eq!(children_range, 1..2);
        assert!(api.is_empty());
        assert!(tests.is_empty());
    }

    #[test]
    fn everything_fits_in_the_router_when_it_can() {
        let children = [child("a")];
        let plan = ScopePlan::build(&ScopePath::Root, records(&children), 250 * 4);
        assert!(plan.pages().is_empty());
        assert_eq!(plan.router().records, 0..1);
        assert!(!plan.is_over_budget());
    }
}
