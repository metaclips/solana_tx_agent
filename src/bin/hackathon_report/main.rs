mod agent;
mod config;
mod report;
mod transactions;
mod types;
mod util;

use anyhow::bail;

use crate::{config::ReportConfig, report::write_report};

fn main() -> anyhow::Result<()> {
    let config = ReportConfig::from_args()?;
    let report = report::run_report(config.clone())?;
    write_report(&config.report_path, &report)?;

    println!(
        "wrote hackathon report to {} ({} submissions, {} failures)",
        config.report_path.display(),
        report.summary.total_submissions,
        report.summary.observed_failures
    );

    if report.summary.total_submissions != 10 || report.summary.observed_failures < 2 {
        bail!(
            "report did not satisfy lifecycle requirement: expected 10 submissions and at least 2 failures"
        );
    }

    Ok(())
}
