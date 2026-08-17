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

/// The sections in page order, which is the order every `[_; 3]` here is in.
const SECTIONS: [Section; 3] = [Section::Children, Section::Api, Section::Tests];

/// Who gets capacity nobody else needed: public symbols, then tests, then
/// navigation.
const LEFTOVER_ORDER: [usize; 3] = [1, 2, 0];

/// Who gives a record back when the page still does not fit: the same order
/// read backwards, so what was handed out last is taken away first.
const SHED_ORDER: [usize; 3] = [0, 2, 1];

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
    ///
    /// The body it plans stays within `byte_budget` whenever any body can:
    /// the only overrun left is the one no plan avoids, and it is flagged
    /// rather than written in silence.
    pub(crate) fn build(scope: &ScopePath, records: Records<'_>, byte_budget: usize) -> Self {
        let capacity = byte_budget.saturating_sub(render::router_overhead(scope));
        if fits_whole(records, capacity) {
            return Self::complete(records);
        }
        Self::truncate(records, capacity)
    }

    /// True when no plan of this scope fits the budget.
    ///
    /// Distinct from ordinary truncation: a record that fits the page but not
    /// its section's share is omitted and stated, and the scope is fine. Two
    /// things land here instead. A record too large for the whole page still
    /// has to be named, so the remainder line covers it. And a budget too
    /// small for the remainder lines themselves has no plan at all — stating
    /// what was dropped is not free, and a page that says nothing about its
    /// own gaps is the one outcome this module will not produce. Both mean the
    /// same thing to a reader, and the report says so.
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
    ///
    /// The split is only a proposal. What has to fit is the page, so the three
    /// sections are measured together afterwards and shrunk until they do.
    fn truncate(records: Records<'_>, capacity: usize) -> Self {
        let needed: [usize; 3] = SECTIONS
            .map(|section| section_bytes(records, section, records.section_len(section), 0));
        let equal = capacity / 3;
        let mut alloc = needed.map(|need| need.min(equal));
        let mut leftover = capacity.saturating_sub(alloc.iter().sum());
        for index in LEFTOVER_ORDER {
            let extra = needed[index].saturating_sub(alloc[index]);
            let give = extra.min(leftover);
            alloc[index] += give;
            leftover -= give;
        }

        let mut plans =
            [0, 1, 2].map(|index| keep_prefix(records, SECTIONS[index], alloc[index], capacity));
        let mut over_budget = plans.iter().any(|plan| plan.over_budget);

        while plans.iter().map(|plan| plan.bytes).sum::<usize>() > capacity {
            let Some(index) = SHED_ORDER.into_iter().find(|&index| plans[index].kept > 0) else {

                over_budget = true;
                break;
            };
            plans[index].shrink(records, SECTIONS[index]);
        }

        Self {
            router: PageContent {
                children: 0..plans[0].kept,
                api: 0..plans[1].kept,
                tests: 0..plans[2].kept,
                omitted_children: plans[0].omitted,
                omitted_api: plans[1].omitted,
                omitted_tests: plans[2].omitted,
            },
            over_budget,
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
///
/// A record never costs negative bytes, so the running total is highest once
/// every record is on the page: one comparison at the end answers the same
/// question as one per record.
fn fits_whole(records: Records<'_>, capacity: usize) -> bool {
    let mut sizer = render::BodySizer::new(records);
    for index in 0..records.len() {
        sizer.push(index);
    }
    sizer.bytes() <= capacity
}

/// What one section's share of the capacity bought.
///
/// Carries its own measured size, because the share is only a proposal: the
/// page is checked, and shrunk, against the sum of the three.
struct SectionPlan {
    kept: usize,
    omitted: usize,
    /// Body bytes this section costs, remainder line included.
    bytes: usize,
    /// Whether this section's first record alone exceeds the whole page.
    over_budget: bool,
}

impl SectionPlan {
    /// Gives the last kept record back.
    ///
    /// Always cheaper than keeping it: counting one more omission grows the
    /// remainder line by at most a digit, and no rendered record line is one
    /// byte long. Dropping the last one drops the line entirely, since a
    /// section that keeps nothing states its remainder in place of `_None._`.
    fn shrink(&mut self, records: Records<'_>, section: Section) {
        self.kept -= 1;
        self.omitted += 1;
        self.bytes = section_bytes(records, section, self.kept, self.omitted);
    }
}

/// The longest prefix of `section` that fits in `budget`, and what it costs.
///
/// The sizer carries the prefix it has already measured from one candidate to
/// the next, so this costs one rendered line per record rather than one per
/// record *per candidate*. Only the remainder line has to be re-priced each
/// time, because its digit count shrinks as the prefix grows.
fn keep_prefix(
    records: Records<'_>,
    section: Section,
    budget: usize,
    page_capacity: usize,
) -> SectionPlan {
    let total = records.section_len(section);
    if total == 0 {
        return SectionPlan {
            kept: 0,
            omitted: 0,
            bytes: 0,
            over_budget: false,
        };
    }
    let start = records.section_start(section);
    let mut sizer = render::BodySizer::new(records);

    let mut first = sizer.clone();
    first.push(start);
    let over_budget = first.bytes() > page_capacity;

    let mut kept = 0;
    while kept < total {
        let mut next = sizer.clone();
        next.push(start + kept);
        if next.bytes() + omission_cost(section, kept + 1, total - kept - 1) > budget {
            break;
        }
        sizer = next;
        kept += 1;
    }
    SectionPlan {
        kept,
        omitted: total - kept,
        bytes: sizer.bytes() + omission_cost(section, kept, total - kept),
        over_budget,
    }
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

    fn test(name: &str) -> TestRecord {
        TestRecord {
            path: format!("tests/{name}.rs"),
            file_name: format!("{name}.rs"),
            name: None,
            anchor_name: None,
            kind: None,
            signature: None,
            start_line: 1,
        }
    }

    /// What the plan's own sizer says the rendered body costs.
    fn planned_bytes(records: Records<'_>, content: &PageContent) -> usize {
        section_bytes(
            records,
            Section::Children,
            content.children.len(),
            content.omitted_children,
        ) + section_bytes(
            records,
            Section::Api,
            content.api.len(),
            content.omitted_api,
        ) + section_bytes(
            records,
            Section::Tests,
            content.tests.len(),
            content.omitted_tests,
        )
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

    /// A page either fits the budget it was planned against or says it could
    /// not. Nothing in between: a body that quietly overran would make the
    /// `budget` in its own frontmatter a claim the file disproves.
    ///
    /// Swept rather than sampled, because the failure lives at the budgets
    /// where a section affords its remainder line but not one record — a
    /// narrow band no hand-picked number reliably lands in.
    #[test]
    fn a_page_that_cannot_fit_its_budget_says_so() {
        let children: Vec<ChildRecord> = (0..8)
            .map(|index| child(&format!("child{index:02}")))
            .collect();
        let tests: Vec<TestRecord> = (0..8)
            .map(|index| test(&format!("suite{index:02}")))
            .collect();
        let all = Records {
            children: &children,
            api: &[],
            tests: &tests,
        };

        let overhead = render::router_overhead(&ScopePath::Root);
        for budget in overhead..overhead + 512 {
            let plan = ScopePlan::build(&ScopePath::Root, all, budget);
            let capacity = budget - overhead;
            let bytes = planned_bytes(all, plan.router());
            assert!(
                bytes <= capacity || plan.is_over_budget(),
                "budget {budget}: a body of {bytes} bytes overran a capacity of \
                 {capacity} without reporting it"
            );
            assert_eq!(
                plan.router().children.end + plan.router().omitted_children,
                children.len(),
                "budget {budget}: children went unaccounted for"
            );
            assert_eq!(
                plan.router().tests.end + plan.router().omitted_tests,
                tests.len(),
                "budget {budget}: tests went unaccounted for"
            );
        }
    }
}
