//! Opt-in native GUI measurements. No data is collected unless the output
//! path is explicitly set; samples are written once on normal application exit.
use std::{io::Write, path::PathBuf, time::Instant};

pub struct PlotMeasurements {
    path: Option<PathBuf>,
    start: Instant,
    rows: Vec<String>,
    pub prepare_ms: f64,
    pub rendered: [usize; 2],
}

impl Default for PlotMeasurements {
    fn default() -> Self {
        Self {
            path: std::env::var_os("LANGMUIR_PLOT_MEASUREMENTS").map(PathBuf::from),
            start: Instant::now(),
            rows: Vec::new(),
            prepare_ms: 0.0,
            rendered: [0; 2],
        }
    }
}

impl PlotMeasurements {
    pub fn record(&mut self, update: Instant, cache: [usize; 2], plotted: u64) {
        if self.path.is_some() && self.rows.len() < 30_000 {
            self.rows.push(format!(
                "{:.6},{:.6},{:.6},{},{},{},{},{}\n",
                self.start.elapsed().as_secs_f64(),
                update.elapsed().as_secs_f64() * 1000.0,
                self.prepare_ms,
                cache[0],
                cache[1],
                self.rendered[0],
                self.rendered[1],
                plotted
            ));
        }
    }
}

impl Drop for PlotMeasurements {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let result = (|| -> std::io::Result<()> {
                let mut output = std::io::BufWriter::new(std::fs::File::create(path)?);
                output.write_all(
                    b"seconds,update_ms,prepare_ms,cache_1,cache_2,render_1,render_2,plotted\n",
                )?;
                for row in &self.rows {
                    output.write_all(row.as_bytes())?;
                }
                output.flush()
            })();
            if let Err(error) = result {
                eprintln!("GUI measurement output: {error}");
            }
        }
    }
}
