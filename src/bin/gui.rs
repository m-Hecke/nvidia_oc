use eframe::{egui, epi};
use nvml_wrapper::{Nvml, Device};
use nvml_wrapper::enums::device::{GpuLockedClocksSetting, Clock};
use std::path::PathBuf;
use std::{fs::OpenOptions, io::Write};
use std::process::Command;

fn documents_dir() -> PathBuf {
    let mut path = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    path.push("Documents");
    path
}

#[derive(Clone)]
struct Record {
    power_limit: u32,
    freq_offset: i32,
    mem_offset: i32,
    min_clock: u32,
    max_clock: u32,
    score: f32,
    avg_power: f32,
}

#[derive(Default, Clone)]
struct SupportedClocks {
    graphics: Vec<u32>,
    memory: Vec<u32>,
}

fn query_supported_clocks() -> Option<SupportedClocks> {
    let output = Command::new("nvidia-smi")
        .args(["-q", "-d", "SUPPORTED_CLOCKS"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut clocks = SupportedClocks::default();
    let mut mode = "";
    for line in stdout.lines() {
        let t = line.trim();
        if t.starts_with("Graphics") {
            mode = "g";
            continue;
        }
        if t.starts_with("Memory") {
            mode = "m";
            continue;
        }
        if let Some(val) = t.strip_suffix("MHz") {
            if let Ok(num) = val.trim().parse::<u32>() {
                match mode {
                    "g" => clocks.graphics.push(num),
                    "m" => clocks.memory.push(num),
                    _ => {}
                }
            }
        }
    }
    Some(clocks)
}

struct GuiApp {
    nvml: Option<Nvml>,
    records: Vec<Record>,
    running: bool,
    supported: Option<SupportedClocks>,
}

impl Default for GuiApp {
    fn default() -> Self {
        Self { nvml: None, records: Vec::new(), running: false, supported: None }
    }
}

impl epi::App for GuiApp {
    fn name(&self) -> &str { "NVIDIA Undervolt" }

    fn setup(&mut self, ctx: &eframe::CreationContext<'_>) {
        if let Ok(nvml) = Nvml::init() {
            self.nvml = Some(nvml);
        }
        self.supported = query_supported_clocks();
        if let Some(ref style) = ctx.egui_ctx.style().visuals.widgets.active {
            let mut style = ctx.egui_ctx.style().clone();
            style.visuals = egui::Visuals::dark();
            ctx.egui_ctx.set_style(style);
        }
    }

    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.running {
                ui.label("Benchmark running...");
            } else {
                if ui.button("Start Undervolt Search").clicked() {
                    if let Some(ref nvml) = self.nvml {
                        if let Ok(mut device) = nvml.device_by_index(0) {
                            self.running = true;
                            self.records.clear();
                            let default_limit = device.enforced_power_limit().unwrap_or(0);
                            let default_freq_offset = device.gpc_clock_vf_offset().unwrap_or(0);
                            let default_mem_offset = device.mem_clock_vf_offset().unwrap_or(0);
                            let base_graphics = device.clock_info(Clock::Graphics).unwrap_or(0);
                            let base_memory = device.clock_info(Clock::Memory).unwrap_or(0);
                            let default_clock = device.max_clock_info(Clock::Graphics).unwrap_or(0);

                            let freq_steps: Vec<i32> = self
                                .supported
                                .as_ref()
                                .map(|s| {
                                    s.graphics
                                        .iter()
                                        .rev()
                                        .filter(|&&c| c <= base_graphics)
                                        .map(|&c| c as i32 - base_graphics as i32)
                                        .collect()
                                })
                                .unwrap_or_default();
                            let mem_steps: Vec<i32> = self
                                .supported
                                .as_ref()
                                .map(|s| {
                                    s.memory
                                        .iter()
                                        .rev()
                                        .filter(|&&c| c <= base_memory)
                                        .map(|&c| c as i32 - base_memory as i32)
                                        .collect()
                                })
                                .unwrap_or_default();
                            let clock_steps: Vec<u32> = self
                                .supported
                                .as_ref()
                                .map(|s| {
                                    s.graphics
                                        .iter()
                                        .rev()
                                        .filter(|&&c| c <= default_clock)
                                        .cloned()
                                        .collect()
                                })
                                .unwrap_or_default();

                            let mut limit = default_limit;
                            let mut freq = default_freq_offset;
                            let mut mem = default_mem_offset;
                            let mut max_clock = default_clock;
                            let min_clock = 0u32;

                            let step_power = 5_000;

                            let iterations = freq_steps.len().min(mem_steps.len()).min(clock_steps.len());

                            for i in 0..iterations {
                                if limit < step_power {
                                    break;
                                }
                                limit -= step_power;
                                freq = default_freq_offset + freq_steps[i];
                                mem = default_mem_offset + mem_steps[i];
                                max_clock = clock_steps[i];

                                if device.set_power_management_limit(limit).is_err()
                                    || device.set_gpc_clock_vf_offset(freq).is_err()
                                    || device.set_mem_clock_vf_offset(mem).is_err()
                                    || device
                                        .set_gpu_locked_clocks(GpuLockedClocksSetting::Numeric {
                                            min_clock_mhz: min_clock,
                                            max_clock_mhz: max_clock,
                                        })
                                        .is_err()
                                {
                                    break;
                                }

                                if let Some(res) = run_benchmark(&mut device) {
                                    self.records.push(Record {
                                        power_limit: limit,
                                        freq_offset: freq,
                                        mem_offset: mem,
                                        min_clock,
                                        max_clock,
                                        score: res.score,
                                        avg_power: res.avg_power,
                                    });
                                    save_record(self.records.last().unwrap());
                                } else {
                                    // revert and stop
                                    let _ = device.set_power_management_limit(default_limit);
                                    let _ = device.set_gpc_clock_vf_offset(default_freq_offset);
                                    let _ = device.set_mem_clock_vf_offset(default_mem_offset);
                                    let _ = device.set_gpu_locked_clocks(GpuLockedClocksSetting::Numeric {
                                        min_clock_mhz: min_clock,
                                        max_clock_mhz: default_clock,
                                    });
                                    break;
                                }
                            }

                            let _ = device.set_power_management_limit(default_limit);
                            let _ = device.set_gpc_clock_vf_offset(default_freq_offset);
                            let _ = device.set_mem_clock_vf_offset(default_mem_offset);
                            let _ = device.set_gpu_locked_clocks(GpuLockedClocksSetting::Numeric {
                                min_clock_mhz: min_clock,
                                max_clock_mhz: default_clock,
                            });
                            self.running = false;
                        }
                    }
                }
            }

            egui::plot::Plot::new("results").show(ui, |plot_ui| {
                let points: Vec<_> = self.records.iter().map(|r| egui::plot::PlotPoint::new(r.power_limit as f64/1000.0, r.score as f64)).collect();
                plot_ui.points(egui::plot::Points::new(points));
            });

            if let Some(record) = self.records.last() {
                ui.label(format!(
                    "Last result - PL: {}W, Freq: {} MHz, Mem: {} MHz, Clocks: {}-{} MHz, Score: {:.0}, Avg Power: {:.2}W",
                    record.power_limit / 1000,
                    record.freq_offset,
                    record.mem_offset,
                    record.min_clock,
                    record.max_clock,
                    record.score,
                    record.avg_power
                ));
            }
        });
    }
}

struct BenchResult { score: f32, avg_power: f32 }

fn run_benchmark(_device: &mut Device) -> Option<BenchResult> {
    // Placeholder: run your preferred benchmark here for ~5 minutes
    // Return None if system becomes unstable
    Some(BenchResult { score: 0.0, avg_power: 0.0 })
}

fn save_record(record: &Record) {
    let mut path = documents_dir();
    path.push("nvidia_oc_results.csv");
    let new_file = !path.exists();
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        if new_file {
            let _ = writeln!(file, "power_limit_w,freq_offset,mem_offset,min_clock,max_clock,score,avg_power_w");
        }
        let _ = writeln!(
            file,
            "{},{},{},{},{},{:.0},{:.2}",
            record.power_limit / 1000,
            record.freq_offset,
            record.mem_offset,
            record.min_clock,
            record.max_clock,
            record.score,
            record.avg_power
        );
    }
}

fn main() {
    let options = eframe::NativeOptions::default();
    eframe::run_native(Box::new(GuiApp::default()), options);
}

