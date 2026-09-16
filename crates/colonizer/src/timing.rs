//! Phase timings for a colony launch.
//!
//! Colony start-up is five or six things in a row — cloning, pulling an image,
//! booting, joining the mesh, waiting for the agent — and until now the only
//! number anyone had was the total, by stopwatch. That is not enough to decide
//! what to optimise: pre-pulling an image only helps a cold image, and a warm
//! pool only helps once the image is already local.
//!
//! A [`Phases`] recorder is threaded through `boot_inner`. Each phase is closed
//! by the next `mark`, so the phases partition the boot with no gaps and no
//! double counting. The result is logged as one line and kept on the session so
//! the API can return it.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One named span of a colony launch, in the order it happened.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub ms: u64,
}

/// Collects phase durations across one launch.
///
/// `mark` closes the phase that was open and opens the next one, so callers
/// name a phase when they *finish* it rather than tracking start and end pairs.
pub struct Phases {
    started: Instant,
    open: Instant,
    phases: Vec<Phase>,
}

impl Phases {
    pub fn new() -> Self {
        let now = Instant::now();
        Self { started: now, open: now, phases: Vec::new() }
    }

    /// Closes the open phase under `name` and starts the next one.
    ///
    /// A phase that took no measurable time is still recorded: "0 ms" is a
    /// finding, and a missing row would look like the phase never ran.
    pub fn mark(&mut self, name: &str) {
        let now = Instant::now();
        self.phases.push(Phase { name: name.to_string(), ms: now.duration_since(self.open).as_millis() as u64 });
        self.open = now;
    }

    /// Wall clock from construction to now.
    pub fn total_ms(&self) -> u64 {
        Instant::now().duration_since(self.started).as_millis() as u64
    }

    #[cfg(test)]
    pub fn phases(&self) -> &[Phase] {
        &self.phases
    }

    /// `boot 12345 ms: clone 800, image 9000, vm-boot 1200, mesh 900, agentd 445`
    ///
    /// One line, ordered, so a support question can be answered by pasting a log
    /// rather than by asking someone to reproduce it.
    pub fn summary(&self) -> String {
        let parts: Vec<String> = self.phases.iter().map(|p| format!("{} {}", p.name, p.ms)).collect();
        format!("boot {} ms: {}", self.total_ms(), parts.join(", "))
    }

    /// `{total_ms, phases: [{name, ms}]}`, for the session record and the API.
    pub fn to_json(&self) -> Value {
        json!({ "total_ms": self.total_ms(), "phases": self.phases })
    }
}

impl Default for Phases {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_partition_the_boot() {
        let mut p = Phases::new();
        p.mark("clone");
        p.mark("image");
        p.mark("vm-boot");
        let names: Vec<&str> = p.phases().iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["clone", "image", "vm-boot"]);
        // Phases are consecutive spans, so they can never exceed the total.
        let sum: u64 = p.phases().iter().map(|x| x.ms).sum();
        assert!(sum <= p.total_ms(), "phases {sum} ms exceed total {} ms", p.total_ms());
    }

    #[test]
    fn a_zero_length_phase_is_still_recorded() {
        let mut p = Phases::new();
        p.mark("skipped");
        assert_eq!(p.phases().len(), 1, "a phase that did nothing must still appear");
    }

    #[test]
    fn summary_names_every_phase_in_order() {
        let mut p = Phases::new();
        p.mark("clone");
        p.mark("image");
        let s = p.summary();
        assert!(s.starts_with("boot "), "{s}");
        let clone_at = s.find("clone").expect("clone in summary");
        let image_at = s.find("image").expect("image in summary");
        assert!(clone_at < image_at, "phases out of order: {s}");
    }

    #[test]
    fn json_carries_total_and_every_phase() {
        let mut p = Phases::new();
        p.mark("clone");
        let v = p.to_json();
        assert!(v["total_ms"].is_u64());
        assert_eq!(v["phases"].as_array().expect("phases array").len(), 1);
        assert_eq!(v["phases"][0]["name"], "clone");
    }
}
