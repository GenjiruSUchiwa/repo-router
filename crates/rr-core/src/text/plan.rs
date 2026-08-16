//! The deterministic, budgeted page planner.
//!
//! A directory whose records do not fit one page keeps a prefix of each
//! section and states the remainder on that section's last line. The budget
//! decides what a reader has to hold, never how many files exist: every
//! indexed scope is one `MAP.md`.
//!
//! Truncation is per section, never over the flattened `Children` / `API` /
//! `Tests` prefix. Spending the whole budget on children first is how a
//! `tests/` tree becomes a map of subdirectory links with `## API _None._`.
//!
//! Sizes come from [`super::render`], the code that writes the bytes, not from
//! a second estimate that could drift.

use std::ops::Range;

use super::digest::HashStream;
use super::model::{ApiRecord, ChildRecord, ScopePath, TestRecord};
use super::render::{self, OmittedKind};

/// The three record lists of one scope, in the order they appear on a page.
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

/// One of the three rendered sections, in page order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Children,
    Api,
    Tests,
}

impl Records<'_> {
    pub(crate) fn len(&self) -> usize {
        self.children.len() + self.api.len() + self.tests.len()
    }

    /// Resolves a flattened index into the list that holds it.
    ///
    /// The flattened order is `Children`, `API`, `Tests` — the same order the
    /// sections appear in a rendered page.
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

    fn section_len(self, section: Section) -> usize {
        match section {
            Section::Children => self.children.len(),
            Section::Api => self.api.len(),
            Section::Tests => self.tests.len(),
        }
    }

    fn section_start(self, section: Section) -> usize {
        match section {
            Section::Children => 0,
            Section::Api => self.children.len(),
            Section::Tests => self.children.len() + self.api.len(),
        }
    }
}

/// What one router holds.
///
/// Each section is a prefix of that section's records plus a count of what
/// the budget dropped. There is no flattened range: a contiguous run over
/// `Children` then `API` then `Tests` is exactly the packing this planner
/// refuses, because it spends the budget on navigation first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PageContent {
    pub(crate) children: Range<usize>,
    pub(crate) api: Range<usize>,
    pub(crate) tests: Range<usize>,
    pub(crate) omitted_children: usize,
    pub(crate) omitted_api: usize,
    pub(crate) omitted_tests: usize,
}

/// The frozen page plan of one directory scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopePlan {
    router: PageContent,
    over_budget: bool,
}

impl ScopePlan {
    /// Plans one scope against a body-byte budget.
    ///
    /// Never fails. A record too large to fit the page is omitted and sets
    /// [`Self::is_over_budget`]; refusing instead would make one oversize
    /// signature block the whole repository's map from ever being written.
    pub(crate) fn build(scope: &ScopePath, records: Records<'_>, byte_budget: usize) -> Self {
        let capacity = byte_budget.saturating_sub(render::router_overhead(scope));
        if fits_whole(records, capacity) {
            return Self::complete(records);
        }
        Self::truncate(records, capacity)
    }

    /// True when some record alone exceeds the whole page.
    ///
    /// Distinct from ordinary truncation: a record that fits the page but not
    /// its section's share is omitted and stated, and the scope is fine. A
    /// record that cannot fit anywhere still has to be named, so the remainder
    /// line covers it and the report says so.
    pub(crate) const fn is_over_budget(&self) -> bool {
        self.over_budget
    }

    pub(crate) const fn router(&self) -> &PageContent {
        &self.router
    }

    /// The plan's contribution to `index_hash`.
    ///
    /// The plan is hashed because it decides what is in the one file: two
    /// projections that keep different prefixes of the same records are not
    /// interchangeable.
    pub(crate) fn write_index_hash(&self, stream: &mut HashStream) {
        write_content_hash(&self.router, stream);
    }

    fn complete(records: Records<'_>) -> Self {
        Self {
            router: PageContent {
                children: 0..records.children.len(),
                api: 0..records.api.len(),
                tests: 0..records.tests.len(),
                omitted_children: 0,
                omitted_api: 0,
                omitted_tests: 0,
            },
            over_budget: false,
        }
    }

    /// Each section gets an equal third of the capacity; unused thirds go to
    /// `API`, then `Tests`, then `Children`.
    ///
    /// Equal first so a child-heavy `tests/` still shows some API, and leftover
    /// second so an API-heavy module is not capped at a third while its
    /// `## Children` and `## Tests` sit empty. The leftover order is the
    /// content order Radar names — public symbols, then tests, then
    /// navigation.
    fn truncate(records: Records<'_>, capacity: usize) -> Self {
        let sections = [Section::Children, Section::Api, Section::Tests];
        let needed: [usize; 3] = sections
            .map(|section| section_bytes(records, section, records.section_len(section), 0));
        let equal = capacity / 3;
        let mut alloc = [0_usize; 3];
        for (index, need) in needed.iter().enumerate() {
            alloc[index] = (*need).min(equal);
        }
        let mut leftover = capacity.saturating_sub(alloc.iter().sum());
        for index in [1, 2, 0] {
            let extra = needed[index].saturating_sub(alloc[index]);
            let give = extra.min(leftover);
            alloc[index] += give;
            leftover -= give;
        }

        let (children, omitted_children, over_children) =
            keep_prefix(records, Section::Children, alloc[0], capacity);
        let (api, omitted_api, over_api) = keep_prefix(records, Section::Api, alloc[1], capacity);
        let (tests, omitted_tests, over_tests) =
            keep_prefix(records, Section::Tests, alloc[2], capacity);

        Self {
            router: PageContent {
                children: 0..children,
                api: 0..api,
                tests: 0..tests,
                omitted_children,
                omitted_api,
                omitted_tests,
            },
            over_budget: over_children || over_api || over_tests,
        }
    }
}

fn write_content_hash(content: &PageContent, stream: &mut HashStream) {
    stream.count(content.children.start);
    stream.count(content.children.end);
    stream.count(content.api.start);
    stream.count(content.api.end);
    stream.count(content.tests.start);
    stream.count(content.tests.end);
    stream.count(content.omitted_children);
    stream.count(content.omitted_api);
    stream.count(content.omitted_tests);
}

/// Whether every record fits in one page with nothing omitted.
fn fits_whole(records: Records<'_>, capacity: usize) -> bool {
    let mut sizer = render::BodySizer::new(records);
    for index in 0..records.len() {
        if !sizer.would_fit(index, capacity) {
            return false;
        }
        sizer.push(index);
    }
    true
}

/// The longest prefix of `section` that fits in `budget`, plus whether its
/// first record exceeds the whole page.
fn keep_prefix(
    records: Records<'_>,
    section: Section,
    budget: usize,
    page_capacity: usize,
) -> (usize, usize, bool) {
    let total = records.section_len(section);
    if total == 0 {
        return (0, 0, false);
    }
    let over_budget = section_bytes(records, section, 1, 0) > page_capacity;
    let mut kept = 0;
    while kept < total {
        let next = kept + 1;
        let omitted = total - next;
        if section_bytes(records, section, next, omitted) > budget {
            break;
        }
        kept = next;
    }
    (kept, total - kept, over_budget)
}

/// Body bytes `kept` records of this section add, including an omission line
/// when `omitted` is not zero.
fn section_bytes(records: Records<'_>, section: Section, kept: usize, omitted: usize) -> usize {
    let mut sizer = render::BodySizer::new(records);
    let start = records.section_start(section);
    for index in start..start + kept {
        sizer.push(index);
    }
    sizer.bytes() + omission_cost(section, kept, omitted)
}

fn omission_cost(section: Section, kept: usize, omitted: usize) -> usize {
    if omitted == 0 {
        return 0;
    }
    let line = render::omission_line(omitted_kind(section), omitted).len() + 1;
    if kept == 0 {
        line.saturating_sub(render::EMPTY_SECTION.len() + 1)
    } else {
        line
    }
}

const fn omitted_kind(section: Section) -> OmittedKind {
    match section {
        Section::Children => OmittedKind::Children,
        Section::Api => OmittedKind::Api,
        Section::Tests => OmittedKind::Tests,
    }
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
    fn a_flattened_range_resolves_into_its_section() {
        let children = [child("a"), child("b")];
        let all = records(&children);
        assert_eq!(all.at(0), Slot::Child(0));
        assert_eq!(all.at(1), Slot::Child(1));
        assert_eq!(all.section_len(Section::Children), 2);
        assert_eq!(all.section_start(Section::Api), 2);
    }

    #[test]
    fn everything_fits_in_the_router_when_it_can() {
        let children = [child("a")];
        let plan = ScopePlan::build(&ScopePath::Root, records(&children), 250 * 4);
        assert_eq!(plan.router().children, 0..1);
        assert_eq!(plan.router().omitted_children, 0);
        assert!(!plan.is_over_budget());
    }

    #[test]
    fn a_tight_budget_keeps_a_prefix_and_counts_the_rest() {
        let children: Vec<ChildRecord> = (0..20)
            .map(|index| child(&format!("dir{index:02}")))
            .collect();
        let plan = ScopePlan::build(&ScopePath::Root, records(&children), 80);
        assert!(
            plan.router().omitted_children > 0,
            "twenty children fit a tiny budget"
        );
        assert_eq!(
            plan.router().children.end + plan.router().omitted_children,
            20
        );
        assert!(plan.router().api.is_empty());
        assert!(plan.router().tests.is_empty());
    }
}
