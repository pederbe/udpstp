//! The Server-scale run loop, with clocks, hosts and files supplied by
//! its caller. The order and log words are `scale.py`'s `run_matrix` and
//! `calibrate`; the generic re-run queue lives in `bench-rounds`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use bench_rounds::order::{self, Queue};
use bench_rounds::py::{EMPTY_DICT, Json};
use indexmap::IndexMap;

use super::Error;
use super::judge::{self, PointKey, display};
use super::matrix::{Matrix, ScaleRun, dict, integer, list, text};

const RERUN_GAP: usize = 5;
const EASY_IN_A_ROW: usize = 3;
const TOOLING_IN_A_ROW: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Run(ScaleRun),
    /// Persist `scale/confinement.json` before any probe or curve runs.
    SaveConfinement(Json),
    OutOfTime,
    Stopped(String),
    Done,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Next {
    pub step: Step,
    /// Emit these in order before carrying out the step.
    pub logs: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Calibrate,
    Probe,
    Matrix,
    Extra,
    Done,
}

pub struct Schedule {
    plan: Json,
    runs: Vec<ScaleRun>,
    phase: Phase,
    awaiting: Option<ScaleRun>,
    candidates: VecDeque<String>,
    candidate: Option<String>,
    calibrated_cases: Vec<String>,
    calibration: VecDeque<ScaleRun>,
    calibrated: Vec<Json>,
    probe: VecDeque<ScaleRun>,
    had_probe: bool,
    queue: Queue<ScaleRun>,
    extra: VecDeque<ScaleRun>,
    lowest: BTreeMap<String, i64>,
    easy: Vec<bool>,
    tooling: Vec<bool>,
    by_engine: BTreeMap<String, Vec<bool>>,
    valid_at: BTreeMap<PointKey, i64>,
    next_index: i64,
    records: Vec<Json>,
    confined: Json,
    logs: Vec<String>,
    stopped: Option<String>,
}

impl Schedule {
    pub fn new(matrix: &Matrix) -> Result<Self, Error> {
        let mut candidates = VecDeque::new();
        for run in &matrix.runs {
            if run.kind == "confine" && !candidates.contains(&run.cpus) {
                candidates.push_back(run.cpus.clone());
            }
        }
        let mut lowest = BTreeMap::new();
        for case in list(matrix.plan.item("cases")?)? {
            let counts = list(case.item("counts")?)?;
            if counts.len() > 1 {
                lowest.insert(
                    text(case.item("name")?)?.into(),
                    counts
                        .iter()
                        .map(integer)
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .min()
                        .expect("two counts"),
                );
            }
        }
        let next_index = matrix
            .runs
            .iter()
            .map(|r| r.index)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error("run index out of range".into()))?;
        let probe: VecDeque<_> = matrix
            .runs
            .iter()
            .filter(|r| r.kind == "probe")
            .cloned()
            .collect();
        Ok(Self {
            plan: matrix.plan.clone(),
            runs: matrix.runs.clone(),
            phase: if candidates.is_empty() {
                Phase::Probe
            } else {
                Phase::Calibrate
            },
            awaiting: None,
            candidates,
            candidate: None,
            calibrated_cases: Vec::new(),
            calibration: VecDeque::new(),
            calibrated: Vec::new(),
            had_probe: !probe.is_empty(),
            probe,
            queue: Queue::new(
                matrix
                    .runs
                    .iter()
                    .filter(|r| !matches!(r.kind.as_str(), "probe" | "confine"))
                    .cloned(),
                RERUN_GAP,
            ),
            extra: VecDeque::new(),
            lowest,
            easy: Vec::new(),
            tooling: Vec::new(),
            by_engine: BTreeMap::new(),
            valid_at: BTreeMap::new(),
            next_index,
            records: Vec::new(),
            confined: EMPTY_DICT.clone(),
            logs: Vec::new(),
            stopped: None,
        })
    }

    pub fn records(&self) -> &[Json] {
        &self.records
    }
    /// The selected logical CPUs, as `case -> cpus`, after calibration.
    pub fn confined(&self) -> &Json {
        &self.confined
    }

    /// Call again only after recording a Run's result. Time is read once
    /// by the caller for this decision; equality with the deadline fits.
    pub fn next(&mut self, now: f64, deadline: f64) -> Result<Next, Error> {
        if self.awaiting.is_some() {
            return Err(Error("a scheduled run still needs its record".into()));
        }
        if let Some(why) = self.stopped.take() {
            self.phase = Phase::Done;
            return Ok(self.finish(Step::Stopped(why)));
        }
        loop {
            let run = match self.phase {
                Phase::Calibrate => {
                    if let Some(run) = self.calibration.pop_front() {
                        Some(run)
                    } else {
                        self.finish_candidate()?;
                        if let Some(candidate) = self.candidates.pop_front() {
                            let verdicts =
                                judge::confinement_verdicts(&self.plan, &self.calibrated)?;
                            let chosen = judge::confinement(&self.plan, &verdicts)?;
                            self.calibration = self
                                .runs
                                .iter()
                                .filter(|r| {
                                    r.kind == "confine"
                                        && r.cpus == candidate
                                        && chosen
                                            .as_dict()
                                            .is_some_and(|c| !c.contains_key(&r.case))
                                })
                                .cloned()
                                .collect();
                            self.calibrated_cases = self
                                .calibration
                                .iter()
                                .map(|r| r.case.clone())
                                .collect::<BTreeSet<_>>()
                                .into_iter()
                                .collect();
                            self.candidate = Some(candidate);
                            continue;
                        }
                        let file = self.finish_calibration()?;
                        self.phase = Phase::Probe;
                        // Candidate verdicts were logged before the file,
                        // selected CPUs after it, as in the reference.
                        let logs = std::mem::take(&mut self.logs);
                        for (case, item) in file.as_dict().expect("calibration object") {
                            self.logs.push(format!(
                                "the calibration step: {case} runs on {}: {}",
                                cpus_named(text(item.item("cpus")?)?),
                                display(item.item("why")?)
                            ));
                        }
                        return Ok(Next {
                            step: Step::SaveConfinement(file),
                            logs,
                        });
                    }
                }
                Phase::Probe => {
                    if let Some(run) = self.probe.pop_front() {
                        Some(run)
                    } else {
                        let probe = self.plan.item("probe")?;
                        let engines = list(probe.item("stop_engines")?)?;
                        let cases = list(probe.item("stop_cases")?)?;
                        if self.had_probe
                            && !engines.is_empty()
                            && judge::probe_stops(&self.records, engines, cases)?
                        {
                            let names = |v: &[Json]| -> Result<String, Error> {
                                Ok(v.iter()
                                    .map(text)
                                    .collect::<Result<Vec<_>, _>>()?
                                    .join(", "))
                            };
                            let why = format!(
                                "the early probe: no top-count run of {} in {} could judge its engine (each was void, other than only for the path's loss, or failed while flagged for what may have caused it); these hosts can't judge the key cases, so the round ends here",
                                names(engines)?,
                                names(cases)?
                            );
                            self.logs.push(why.clone());
                            self.phase = Phase::Done;
                            return Ok(self.finish(Step::Stopped(why)));
                        }
                        self.phase = Phase::Matrix;
                        continue;
                    }
                }
                Phase::Matrix => {
                    if let Some(run) = self.queue.next() {
                        Some(run)
                    } else {
                        self.extra = close_call_runs(
                            &self.plan,
                            &self.records,
                            &self.runs,
                            self.next_index,
                        )?
                        .into();
                        if !self.extra.is_empty() {
                            self.logs.push(format!(
                                "the close-call band: {} more runs",
                                self.extra.len()
                            ));
                        }
                        self.phase = Phase::Extra;
                        continue;
                    }
                }
                Phase::Extra => self.extra.pop_front(),
                Phase::Done => None,
            };
            let Some(run) = run else {
                self.phase = Phase::Done;
                return Ok(self.finish(Step::Done));
            };
            if now + run.seconds() > deadline {
                self.logs.push(
                    if self.phase == Phase::Extra {
                        "out of time: the rest of the close-call band is skipped"
                    } else {
                        "out of time: the rest of the round is skipped"
                    }
                    .into(),
                );
                self.phase = Phase::Done;
                return Ok(self.finish(Step::OutOfTime));
            }
            self.logs.push(run.label());
            self.awaiting = Some(run.clone());
            return Ok(self.finish(Step::Run(run)));
        }
    }

    pub fn record(&mut self, record: Json) -> Result<Vec<String>, Error> {
        let run = self
            .awaiting
            .take()
            .ok_or_else(|| Error("no scheduled run awaits a record".into()))?;
        // The Round's analyser can return a tooling record, but its label
        // must still identify the Run whose host calls just completed.
        if record.item("label")?.as_str() != Some(run.label().as_str()) {
            return Err(Error(
                "a record does not belong to the scheduled run".into(),
            ));
        }
        self.records.push(record.clone());
        if self.phase == Phase::Extra {
            return Ok(Vec::new());
        }
        let valid = record.item("valid")?.truthy();
        let invalid = list(record.item("invalid")?)?;
        let failed = invalid
            .iter()
            .map(text)
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|s| s.starts_with("the tooling"));
        self.tooling.push(failed);
        self.by_engine
            .entry(run.engine.clone())
            .or_default()
            .push(failed);
        match self.phase {
            Phase::Calibrate => {
                self.calibrated.push(record.clone());
                if !valid {
                    self.invalid(&run, invalid)?;
                    if run.rerun == 0 {
                        let again = self.again(&run, false)?;
                        self.calibration.push_back(again);
                    }
                }
            }
            Phase::Probe => {
                let lost = judge::path_loss_text(&record)?;
                if lost.truthy() {
                    self.logs
                        .push(format!("{}: {}", run.label(), display(&lost)));
                }
                return Ok(std::mem::take(&mut self.logs));
            }
            Phase::Matrix => {
                if (run.kind == "p1" || self.lowest.get(&run.case) == Some(&run.count))
                    && !judge::only_the_path(&record)?
                {
                    self.easy.push(valid);
                }
                if valid {
                    *self.valid_at.entry(run.point()).or_default() += 1;
                } else {
                    self.invalid(&run, invalid)?;
                    if run.rerun == 0 {
                        let again = self.again(&run, true)?;
                        self.queue.rerun(again);
                    }
                    let needed = judge::needed_runs(judge::planned_runs(&self.plan, &run.point())?);
                    if self.valid_at.get(&run.point()).copied().unwrap_or(0)
                        + (self.queue.left(|r| r.point() == run.point()) as i64)
                        < needed
                    {
                        self.queue.drop_runs(|r| r.point() == run.point());
                        self.logs.push(format!("{}: the point can no longer reach the {needed} valid runs it needs; its other runs are skipped",run.label()));
                    }
                }
            }
            Phase::Extra | Phase::Done => unreachable!("record belongs to active phase"),
        }
        if let Some(why) = order::stop_now(
            &self.easy,
            &self.tooling,
            &self.by_engine,
            EASY_IN_A_ROW,
            TOOLING_IN_A_ROW,
        ) {
            self.logs
                .push(format!("the stop rule: {why}; the round ends here"));
            self.stopped = Some(why);
        }
        Ok(std::mem::take(&mut self.logs))
    }

    fn finish(&mut self, step: Step) -> Next {
        Next {
            step,
            logs: std::mem::take(&mut self.logs),
        }
    }
    fn invalid(&mut self, run: &ScaleRun, invalid: &[Json]) -> Result<(), Error> {
        self.logs.push(format!(
            "{}: invalid ({})",
            run.label(),
            invalid
                .iter()
                .map(text)
                .collect::<Result<Vec<_>, _>>()?
                .join("; ")
        ));
        Ok(())
    }
    fn again(&mut self, run: &ScaleRun, clear_reserve: bool) -> Result<ScaleRun, Error> {
        let mut again = run.clone();
        again.index = self.next_index;
        again.rerun = 1;
        if clear_reserve {
            again.reserve_s = 0.0;
        }
        self.next_index = self
            .next_index
            .checked_add(1)
            .ok_or_else(|| Error("run index out of range".into()))?;
        Ok(again)
    }
    fn finish_candidate(&mut self) -> Result<(), Error> {
        let Some(candidate) = self.candidate.take() else {
            return Ok(());
        };
        let verdicts = judge::confinement_verdicts(&self.plan, &self.calibrated)?;
        for case in &self.calibrated_cases {
            let v = verdicts.get(case)?.or(&EMPTY_DICT).get(&candidate)?;
            if v.truthy() {
                let why = if v.get("why")?.truthy() {
                    format!(" ({})", display(v.item("why")?))
                } else {
                    String::new()
                };
                self.logs.push(format!("the calibration step: {case} on {}, udpstp alone at {} Test Connections: {}{why}; {} of {} runs valid, {} of them flagged",cpus_named(&candidate),display(self.plan.item("confine")?.item("count")?),display(v.item("verdict")?),display(v.item("valid")?),display(v.item("runs")?),display(v.item("flagged")?)));
            }
        }
        Ok(())
    }
    fn finish_calibration(&mut self) -> Result<Json, Error> {
        let verdicts = judge::confinement_verdicts(&self.plan, &self.calibrated)?;
        let chosen = judge::confinement(&self.plan, &verdicts)?;
        let mut confined = IndexMap::new();
        let mut file = IndexMap::new();
        for (case, pair) in chosen
            .as_dict()
            .ok_or_else(|| Error("invalid confinement choice".into()))?
        {
            confined.insert(case.clone(), pair.at(0)?.clone());
            let verdicts = verdicts
                .get(case)?
                .or(&EMPTY_DICT)
                .as_dict()
                .ok_or_else(|| Error("invalid confinement verdicts".into()))?
                .iter()
                .map(|(cpus, v)| Ok((cpus.clone(), v.item("verdict")?.clone())))
                .collect::<Result<_, Error>>()?;
            file.insert(
                case.clone(),
                dict([
                    ("cpus", pair.at(0)?.clone()),
                    ("why", pair.at(1)?.clone()),
                    ("verdicts", Json::Dict(verdicts)),
                ]),
            );
        }
        self.confined = Json::Dict(confined);
        Ok(Json::Dict(file))
    }
}

fn cpus_named(cpus: &str) -> String {
    format!(
        "logical CPU{} {cpus}",
        if cpus.contains([',', '-']) { "s" } else { "" }
    )
}

pub fn close_call_runs(
    plan: &Json,
    records: &[Json],
    runs: &[ScaleRun],
    first_index: i64,
) -> Result<Vec<ScaleRun>, Error> {
    let band = plan.get_or("close_call", &EMPTY_DICT)?;
    let per_point = integer(band.get_or("runs", &Json::Int(0))?)?;
    let most = integer(band.get_or("most", &Json::Int(0))?)?;
    if per_point == 0 || most == 0 {
        return Ok(Vec::new());
    }
    let mut counted = Vec::new();
    for r in records {
        if r.get("kind")?.as_str() != Some("probe") {
            counted.push(r.clone());
        }
    }
    let verdicts = judge::points(plan, &counted)?;
    let chosen = judge::decision_points(plan, &verdicts)?;
    let mut template = IndexMap::new();
    for run in runs {
        if run.kind == "curve" {
            template.entry(run.point()).or_insert(run);
        }
    }
    let mut extra = Vec::new();
    let mut index = first_index;
    for more in 1..=per_point {
        for key in &chosen {
            if (extra.len() as i64) < most
                && let Some(run) = template.get(key)
            {
                let mut run = (*run).clone();
                run.index = index;
                run.more = more;
                run.rerun = 0;
                run.reserve_s = 0.0;
                extra.push(run);
                index = index
                    .checked_add(1)
                    .ok_or_else(|| Error("run index out of range".into()))?;
            }
        }
    }
    Ok(extra)
}
