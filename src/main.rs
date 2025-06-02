use clap::{arg, Args, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Generator, Shell};
use nvml_wrapper::{Device, Nvml};
use serde::Deserialize;
use std::{collections::HashMap, io};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Path to the config file
    #[arg(short, long, default_value = "/etc/nvidia_oc.json")]
    file: String,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Sets GPU parameters like frequency offset and power limit
    Set {
        /// GPU index
        #[arg(short, long)]
        index: u32,

        #[command(flatten)]
        sets: Sets,
    },
    /// Gets GPU parameters
    Get {
        /// GPU index
        #[arg(short, long)]
        index: u32,
    },
    /// Generate shell completion script
    Completion {
        /// The shell to generate the script for
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Args, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[group(required = true, multiple = true)]
struct Sets {
    /// GPU frequency offset
    #[arg(short, long, allow_hyphen_values = true)]
    freq_offset: Option<i32>,
    /// GPU memory frequency offset
    #[arg(long, allow_hyphen_values = true)]
    mem_offset: Option<i32>,
    /// GPU power limit in milliwatts
    #[arg(short, long)]
    power_limit: Option<u32>,
    /// GPU min clock
    #[arg(long, requires = "max_clock")]
    min_clock: Option<u32>,
    /// GPU max clock
    #[arg(long, requires = "min_clock")]
    max_clock: Option<u32>,
    /// GPU min memory clock
    #[arg(long, requires = "max_mem_clock")]
    min_mem_clock: Option<u32>,
    /// GPU max memory clock
    #[arg(long, requires = "min_mem_clock")]
    max_mem_clock: Option<u32>,
}

impl Sets {
    fn apply(&self, device: &mut Device) {
        if let Some(freq_offset) = self.freq_offset {
            device
                .set_gpc_clock_vf_offset(freq_offset)
                .expect("Failed to set GPU frequency offset");
        }

        if let Some(mem_offset) = self.mem_offset {
            device
                .set_mem_clock_vf_offset(mem_offset)
                .expect("Failed to set GPU memory frequency offset");
        }

        if let Some(limit) = self.power_limit {
            device
                .set_power_management_limit(limit)
                .expect("Failed to set GPU power limit");
        }

        if let (Some(min_clock), Some(max_clock)) = (self.min_clock, self.max_clock) {
            device
                .set_gpu_locked_clocks(
                    nvml_wrapper::enums::device::GpuLockedClocksSetting::Numeric {
                        min_clock_mhz: min_clock,
                        max_clock_mhz: max_clock,
                    },
                )
                .expect("Failed to set GPU min and max clocks");
        }

        if let (Some(min_mem_clock), Some(max_mem_clock)) = (self.min_mem_clock, self.max_mem_clock)
        {
            device
                .set_mem_locked_clocks(min_mem_clock, max_mem_clock)
                .expect("Failed to set GPU min and max memory clocks");
        }
    }
}

#[derive(Deserialize)]
struct Config {
    sets: HashMap<u32, Sets>,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Set { index, sets }) => {
            escalate_permissions().expect("Failed to escalate permissions");

            sudo2::escalate_if_needed()
                .or_else(|_| sudo2::doas())
                .or_else(|_| sudo2::pkexec())
                .expect("Failed to escalate privileges");

            let nvml = Nvml::init().expect("Failed to initialize NVML");

            let mut device = nvml.device_by_index(*index).expect("Failed to get GPU");

            sets.apply(&mut device);
            println!("Successfully set GPU parameters.");
        }
        Some(Commands::Get { index }) => {
            let nvml = Nvml::init().expect("Failed to initialize NVML");
            let device = nvml.device_by_index(*index).expect("Failed to get GPU");

            let freq_offset = device.gpc_clock_vf_offset();
            match freq_offset {
                Ok(freq_offset) => println!("GPU core clock offset: {} MHz", freq_offset),
                Err(e) => eprintln!("Failed to get GPU core clock offset: {:?}", e),
            }

            let mem_offset = device.mem_clock_vf_offset();
            match mem_offset {
                Ok(mem_offset) => println!("GPU memory clock offset: {} MHz", mem_offset),
                Err(e) => eprintln!("Failed to get GPU memory clock offset: {:?}", e),
            }

            let power_limit = device.enforced_power_limit();
            match power_limit {
                Ok(power_limit) => println!("GPU power limit: {} W", power_limit / 1000),
                Err(e) => eprintln!("Failed to get GPU power limit: {:?}", e),
            }
        }
        None => {
            let Ok(config_file) = std::fs::read_to_string(cli.file) else {
                panic!("Configuration file not found and no valid arguments were provided. Run `nvidia_oc --help` for more information.");
            };

            escalate_permissions().expect("Failed to escalate permissions");

            let config: Config =
                serde_json::from_str(&config_file).expect("Invalid configuration file");

            let nvml = Nvml::init().expect("Failed to initialize NVML");

            for (index, sets) in config.sets {
                let mut device = nvml.device_by_index(index).expect("Failed to get GPU");
                sets.apply(&mut device);
            }
            println!("Successfully set GPU parameters.");
        }
        Some(Commands::Completion { shell }) => {
            generate_completion_script(*shell);
        }
    }
}

fn escalate_permissions() -> Result<(), Box<dyn std::error::Error>> {
    if sudo2::running_as_root() {
        return Ok(());
    }

    if which::which("sudo").is_ok() {
        sudo2::escalate_if_needed()?;
    } else if which::which("doas").is_ok() {
        sudo2::doas()?;
    } else if which::which("pkexec").is_ok() {
        sudo2::pkexec()?;
    } else {
        return Err("Please install sudo, doas or pkexec and try again. Alternatively, run the program as root.".into());
    }

    Ok(())
}

fn generate_completion_script<G: Generator>(gen: G) {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(gen, &mut cmd, name, &mut io::stdout());
}
