//! Budgeted assembly of a compact protocol response.
//!
//! A response is a header, a sequence of record units, and — when the
//! character budget forces a cut — a recovery footer. Units are the atoms of
//! truncation: a `hit` record and its `read` recipe are one unit, so the
//! contract in `skills/claude-history/SKILL.md` ("a hit is immediately
//! followed by its read recipe") holds however much is cut. Callers describe
//! their header for the whole and cut cases; this module owns the fitting.

/// What was dropped to fit the budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cut {
    pub kept_units: usize,
    pub omitted_units: usize,
    pub omitted_lines: usize,
}

pub struct Response<'a> {
    pub budget: Option<usize>,
    /// Header (one or more complete lines) for the whole output (`None`) or
    /// for a cut output.
    pub header: &'a dyn Fn(Option<&Cut>) -> String,
    /// Record units in order; each is one or more complete lines.
    pub units: Vec<String>,
    /// Appended only when nothing is cut.
    pub whole_trailer: String,
    /// Recovery record(s) appended after the kept units of a cut output.
    pub cut_footer: &'a dyn Fn(&Cut) -> String,
    /// Used when not even the cut header and footer fit; truncated to the
    /// budget, which the caller reports as `budget-too-small`.
    pub fallback: &'a dyn Fn() -> String,
}

impl Response<'_> {
    pub fn render(self) -> String {
        let whole = (self.header)(None) + &self.units.concat() + &self.whole_trailer;
        let Some(budget) = self.budget else {
            return whole;
        };
        if whole.chars().count() <= budget {
            return whole;
        }

        let total_lines: usize = self.units.iter().map(|unit| unit.lines().count()).sum();
        let mut kept_lines = total_lines;
        for kept in (0..=self.units.len()).rev() {
            let cut = Cut {
                kept_units: kept,
                omitted_units: self.units.len() - kept,
                omitted_lines: total_lines - kept_lines,
            };
            let candidate =
                (self.header)(Some(&cut)) + &self.units[..kept].concat() + &(self.cut_footer)(&cut);
            if candidate.chars().count() <= budget {
                return candidate;
            }
            if kept > 0 {
                kept_lines -= self.units[kept - 1].lines().count();
            }
        }
        (self.fallback)().chars().take(budget).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response<'a>(
        budget: Option<usize>,
        units: Vec<&str>,
        header: &'a dyn Fn(Option<&Cut>) -> String,
        footer: &'a dyn Fn(&Cut) -> String,
        fallback: &'a dyn Fn() -> String,
    ) -> Response<'a> {
        Response {
            budget,
            header,
            units: units.into_iter().map(str::to_owned).collect(),
            whole_trailer: String::new(),
            cut_footer: footer,
            fallback,
        }
    }

    fn header(cut: Option<&Cut>) -> String {
        match cut {
            None => "head cut=none\n".to_owned(),
            Some(cut) => format!("head cut=tail omitted-lines={}\n", cut.omitted_lines),
        }
    }

    fn footer(_: &Cut) -> String {
        "continue\n".to_owned()
    }

    fn fallback() -> String {
        "head cut=tail omitted-lines=all\ncontinue\n".to_owned()
    }

    #[test]
    fn whole_output_when_it_fits_or_no_budget() {
        let units = vec!["a\n", "hit\nread\n"];
        assert_eq!(
            response(None, units.clone(), &header, &footer, &fallback).render(),
            "head cut=none\na\nhit\nread\n"
        );
        assert_eq!(
            response(Some(100), units, &header, &footer, &fallback).render(),
            "head cut=none\na\nhit\nread\n"
        );
    }

    // A 21-char line, a 44-char hit+read unit and a 61-char line; the cut
    // header ("head cut=tail omitted-lines=N\n") is 30 chars, the footer 9.
    const A: &str = "aaaaaaaaaaaaaaaaaaaa\n";
    const HR: &str = "hit xxxxxxxxxxxxxxxxx\nread xxxxxxxxxxxxxxxx\n";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";

    #[test]
    fn drops_trailing_units_whole_and_counts_their_lines() {
        let out = response(Some(60), vec![A, HR, B], &header, &footer, &fallback).render();
        assert_eq!(out, format!("head cut=tail omitted-lines=3\n{A}continue\n"));
    }

    #[test]
    fn keeps_a_multiline_unit_intact() {
        let out = response(Some(83), vec![HR, B], &header, &footer, &fallback).render();
        assert_eq!(
            out,
            format!("head cut=tail omitted-lines=1\n{HR}continue\n")
        );
        // One char short and the whole hit+read unit goes, never half of it.
        let out = response(Some(82), vec![HR, B], &header, &footer, &fallback).render();
        assert_eq!(out, "head cut=tail omitted-lines=3\ncontinue\n");
    }

    #[test]
    fn falls_back_to_a_truncated_minimal_header() {
        let units = vec!["a\n"];
        let out = response(Some(10), units, &header, &footer, &fallback).render();
        assert_eq!(out, "head cut=t");
        assert!(!out.ends_with('\n'));
    }

    #[test]
    fn whole_trailer_only_appears_when_nothing_is_cut() {
        let mut whole = response(Some(100), vec!["a\n"], &header, &footer, &fallback);
        whole.whole_trailer = "warning x\n".to_owned();
        assert_eq!(whole.render(), "head cut=none\na\nwarning x\n");
        let mut cut = response(Some(40), vec!["a\n", "b\n"], &header, &footer, &fallback);
        cut.whole_trailer = "warning that is quite long indeed\n".to_owned();
        assert!(!cut.render().contains("warning"));
    }
}
