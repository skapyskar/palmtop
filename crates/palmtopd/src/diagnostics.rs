//! The `--doctor` report scaffolding shared by every platform's diagnostic
//! checks. Platform-neutral by construction: it renders a list of pass/warn/
//! fail lines and their fixes, and knows nothing about portals, PipeWire,
//! WGC, or ffmpeg -- see `platform::linux::doctor`/`platform::windows::doctor`
//! for the checks themselves.
//!
//! Extracted out of what was originally `doctor.rs`'s own private `Report`/
//! `Check`/`Status` types once a second platform needed the exact same
//! report-rendering behavior -- rather than a second copy of it.

use std::fmt::Write as _;

/// One diagnostic line. `fix` is only shown on failure, so it can be
/// specific and long without cluttering a healthy report.
struct Check {
    name: String,
    status: Status,
    detail: String,
    fix: Option<String>,
}

#[derive(PartialEq)]
enum Status {
    Pass,
    Warn,
    Fail,
}

impl Status {
    fn marker(&self) -> &'static str {
        match self {
            Status::Pass => "  ok  ",
            Status::Warn => " warn ",
            Status::Fail => " FAIL ",
        }
    }
}

pub struct Report {
    checks: Vec<Check>,
}

impl Report {
    pub fn new() -> Self {
        Self { checks: Vec::new() }
    }

    fn add(&mut self, name: &str, status: Status, detail: impl Into<String>, fix: Option<&str>) {
        self.checks.push(Check {
            name: name.to_string(),
            status,
            detail: detail.into(),
            fix: fix.map(|s| s.to_string()),
        });
    }

    pub fn pass(&mut self, name: &str, detail: impl Into<String>) {
        self.add(name, Status::Pass, detail, None);
    }

    pub fn warn(&mut self, name: &str, detail: impl Into<String>, fix: &str) {
        self.add(name, Status::Warn, detail, Some(fix));
    }

    pub fn fail(&mut self, name: &str, detail: impl Into<String>, fix: &str) {
        self.add(name, Status::Fail, detail, Some(fix));
    }

    pub fn failures(&self) -> usize {
        self.checks.iter().filter(|c| c.status == Status::Fail).count()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "\nPalmtop host diagnostics\n");
        for c in &self.checks {
            let _ = writeln!(out, "[{}] {:<22} {}", c.status.marker(), c.name, c.detail);
        }
        let problems: Vec<&Check> =
            self.checks.iter().filter(|c| c.status != Status::Pass).collect();
        if problems.is_empty() {
            let _ = writeln!(
                out,
                "\nEverything checks out. If the phone still shows nothing, the problem is \
                 between the two devices rather than on this machine -- run the daemon in the \
                 foreground (`palmtopd`) and watch its output while the phone connects."
            );
        } else {
            let _ = writeln!(out, "\nWhat to do:\n");
            for c in problems {
                if let Some(fix) = &c.fix {
                    let _ = writeln!(out, "  {}:\n    {}\n", c.name, fix.replace('\n', "\n    "));
                }
            }
        }
        out
    }
}

impl Default for Report {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_with_only_passes_has_zero_failures_and_no_fix_section() {
        let mut r = Report::new();
        r.pass("A", "fine");
        r.pass("B", "also fine");
        assert_eq!(r.failures(), 0);
        assert!(!r.render().contains("What to do"));
        assert!(r.render().contains("Everything checks out"));
    }

    #[test]
    fn warnings_do_not_count_as_failures_but_still_get_a_fix_section() {
        let mut r = Report::new();
        r.pass("A", "fine");
        r.warn("B", "meh", "try this");
        assert_eq!(r.failures(), 0);
        assert!(r.render().contains("What to do"));
        assert!(r.render().contains("try this"));
    }

    #[test]
    fn failures_are_counted_and_their_fix_text_is_rendered() {
        let mut r = Report::new();
        r.fail("A", "broken", "fix A this way");
        r.fail("B", "also broken", "fix B this way");
        assert_eq!(r.failures(), 2);
        let rendered = r.render();
        assert!(rendered.contains("fix A this way"));
        assert!(rendered.contains("fix B this way"));
        assert!(!rendered.contains("Everything checks out"));
    }
}
