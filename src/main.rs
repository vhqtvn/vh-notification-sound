use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::fd::IntoRawFd,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
use libc;

#[derive(Parser, Debug)]
#[command(author, version, 
    about = "A simple application that plays notification sounds while temporarily fading out any currently playing audio. Designed for Linux systems with PulseAudio.", 
    long_about = "A simple application that plays notification sounds while temporarily fading out any currently playing audio.\nDesigned specifically for Linux systems with PulseAudio.\n\nGitHub Repository: https://github.com/vhqtvn/vh-notification-sound")]
struct Args {
    /// Sound alias or path to audio file
    #[arg(index = 1)]
    sound: Option<String>,

    /// Fade out duration in seconds
    #[arg(short, long, env = "VH_NOTIFICATION_FADE_OUT", default_value_t = 0.3)]
    fade_out: f32,

    /// Fade in duration in seconds
    #[arg(short, long, env = "VH_NOTIFICATION_FADE_IN", default_value_t = 0.3)]
    fade_in: f32,

    /// Output volume percentage for notification sound (0-100)
    #[arg(short, long, env = "VH_NOTIFICATION_VOLUME", default_value_t = 75)]
    volume: u8,

    /// Path to config file
    #[arg(short, long, env = "VH_NOTIFICATION_CONFIG")]
    config: Option<PathBuf>,

    /// List available sound aliases from config
    #[arg(short = 'l', long)]
    list_sounds: bool,

    /// Show help information about the application
    #[arg(short = 'h', long)]
    help_info: bool,

    /// Detach process and run in background
    #[arg(short = 'd', long, env = "VH_NOTIFICATION_DETACH")]
    detach: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    fade_out: Option<f32>,
    #[serde(default)]
    fade_in: Option<f32>,
    #[serde(default)]
    volume: Option<u8>,
    #[serde(default)]
    sounds: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fade_out: Some(0.3),
            fade_in: Some(0.3),
            volume: Some(75),
            sounds: HashMap::new(),
        }
    }
}

struct PulseAudioState {
    default_sink: String,
    current_volume: u8,
    unmuted_inputs: Vec<String>,
}

// AudioStateGuard ensures cleanup happens when it goes out of scope
struct AudioStateGuard {
    default_sink: String,
    current_volume: u8,
    unmuted_inputs: Vec<String>,
    cleaned_up: bool,
}

impl AudioStateGuard {
    fn new(state: PulseAudioState) -> Self {
        Self {
            default_sink: state.default_sink,
            current_volume: state.current_volume,
            unmuted_inputs: state.unmuted_inputs,
            cleaned_up: false,
        }
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.cleaned_up {
            return Ok(());
        }

        // Restore original volume
        run_command(
            "pactl",
            &[
                "set-sink-volume",
                &self.default_sink,
                &format!("{}%", self.current_volume),
            ],
        )?;

        // Unmute streams that were unmuted initially
        for input in &self.unmuted_inputs {
            run_command("pactl", &["set-sink-input-mute", input, "0"])?;
        }

        self.cleaned_up = true;
        Ok(())
    }
}

impl Drop for AudioStateGuard {
    fn drop(&mut self) {
        // Attempt cleanup one last time, ignore errors since we can't do much about them during drop
        let _ = self.cleanup();
    }
}

fn main() -> Result<()> {
    // Set up signal handling for clean shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    
    // Handle Ctrl+C
    ctrlc::set_handler(move || {
        eprintln!("Received interrupt signal, cleaning up...");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    let args = Args::parse();
    
    // Load config file if specified or look for default locations
    let config = load_config(&args.config)?;
    
    // Handle help info command
    if args.help_info {
        print_help_info();
        return Ok(());
    }
    
    // Handle list sounds command
    if args.list_sounds {
        print_sound_aliases(&config);
        return Ok(());
    }
    
    // Check if sound is provided
    let sound = match args.sound {
        Some(s) => s,
        None => {
            eprintln!("Error: No sound specified.");
            eprintln!("Usage: vh-notification-sound [OPTIONS] <SOUND>");
            eprintln!("Try 'vh-notification-sound --help' for more information.");
            return Ok(());
        }
    };
    
    // Determine fade durations (priority: args > config > defaults)
    let fade_out = args.fade_out;
    let fade_in = args.fade_in;
    
    // Determine output volume (priority: args > config > defaults)
    let volume = args.volume.min(100);
    
    // Resolve sound path (check if it's an alias in config)
    let sound_path = resolve_sound_path(&sound, &config)?;

    // If detach is enabled, fork the process
    if args.detach {
        match unsafe { libc::fork() } {
            -1 => {
                return Err(anyhow::anyhow!("Failed to fork process"));
            }
            0 => {
                // Child process continues
                // Redirect standard file descriptors to /dev/null
                let null_fd = std::fs::File::open("/dev/null")?.into_raw_fd();
                unsafe {
                    libc::dup2(null_fd, libc::STDIN_FILENO);
                    libc::dup2(null_fd, libc::STDOUT_FILENO);
                    libc::dup2(null_fd, libc::STDERR_FILENO);
                    libc::close(null_fd);
                }
                
                // Create a new session
                if unsafe { libc::setsid() } < 0 {
                    std::process::exit(1);
                }
            }
            _ => {
                // Parent process exits
                return Ok(());
            }
        }
    }

    // Acquire lock to prevent multiple notifications from playing simultaneously
    let lock_path = dirs::runtime_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("vh-notification-sound.lock");

    let lock_file = match acquire_lock(&lock_path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("Another notification is currently playing: {}", e);
            return Ok(());
        }
    };
    
    // Play the notification sound with fade effects
    let result = play_notification(sound_path, fade_out, fade_in, volume, running);
    
    // Release lock
    if let Some(mut file) = lock_file {
        let _ = file.write_all(b"0");
    }
    let _ = std::fs::remove_file(lock_path);
    
    result
}

fn load_config(config_path: &Option<PathBuf>) -> Result<Config> {
    // If config path is provided, use it
    if let Some(path) = config_path {
        if path.exists() {
            let file = std::fs::File::open(path).context("Failed to open config file")?;
            return serde_yaml::from_reader(file).context("Failed to parse config file");
        }
    }
    
    // Check default locations
    let possible_paths = vec![
        PathBuf::from("./vh-notification-sound.yml"),
        dirs::config_dir()
            .map(|p| p.join("vh-notification-sound.yml"))
            .unwrap_or_default(),
        dirs::home_dir()
            .map(|p| p.join(".vh-notification-sound.yml"))
            .unwrap_or_default(),
    ];
    
    for path in possible_paths {
        if path.exists() {
            let file = std::fs::File::open(&path).context("Failed to open config file")?;
            return serde_yaml::from_reader(file).context("Failed to parse config file");
        }
    }
    
    // Return default config if no config file found
    Ok(Config::default())
}

fn resolve_sound_path(sound: &str, config: &Config) -> Result<PathBuf> {
    // Check if the sound is an alias in the config
    if let Some(path) = config.sounds.get(sound) {
        return expand_tilde(path);
    }
    
    // Otherwise, treat it as a direct path
    expand_tilde(sound)
}

fn expand_tilde(path: &str) -> Result<PathBuf> {
    if path.starts_with("~/") || path == "~" {
        let home_dir = dirs::home_dir().context("Could not determine home directory")?;
        if path == "~" {
            Ok(home_dir)
        } else {
            Ok(home_dir.join(&path[2..]))
        }
    } else {
        Ok(PathBuf::from(path))
    }
}

fn run_command(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context(format!("Failed to execute command: {} {:?}", cmd, args))?;
    
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Command failed: {} {:?}\nError: {}", cmd, args, stderr)
    }
}

fn get_pulseaudio_state() -> Result<PulseAudioState> {
    // Get default sink
    let default_sink = run_command("pactl", &["info"])?
        .lines()
        .find(|line| line.contains("Default Sink"))
        .map(|line| line.split(": ").nth(1).unwrap_or("").trim().to_string())
        .context("Failed to get default sink")?;
    
    // Get current volume
    let volume_output = run_command("pactl", &["list", "sinks"])?;
    let current_volume_str = volume_output
        .lines()
        .skip_while(|line| !line.contains(&format!("Name: {}", default_sink)))
        .take(15)
        .find(|line| line.contains("Volume: front-left"))
        .and_then(|line| line.split_whitespace().nth(4))
        .and_then(|vol| vol.trim_end_matches('%').parse::<u8>().ok())
        .context("Failed to get current volume")?;
    
    // Get unmuted sink inputs
    let sink_inputs_output = run_command("pactl", &["list", "short", "sink-inputs"])?;
    let sink_input_ids: Vec<String> = sink_inputs_output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.split_whitespace().next().unwrap_or("").to_string())
        .collect();
    
    let sink_inputs_details = run_command("pactl", &["list", "sink-inputs"])?;
    let mut unmuted_inputs = Vec::new();
    
    for id in sink_input_ids {
        if !id.is_empty() {
            let is_muted = sink_inputs_details
                .lines()
                .skip_while(|line| !line.contains(&format!("Sink Input #{}", id)))
                .take(15)
                .find(|line| line.contains("Mute:"))
                .map(|line| line.contains("yes"))
                .unwrap_or(true);
            
            if !is_muted {
                unmuted_inputs.push(id);
            }
        }
    }
    
    Ok(PulseAudioState {
        default_sink,
        current_volume: current_volume_str,
        unmuted_inputs,
    })
}

fn play_notification(
    sound_path: PathBuf, 
    fade_out: f32, 
    fade_in: f32, 
    volume: u8,
    running: Arc<AtomicBool>
) -> Result<()> {
    // Get current PulseAudio state
    let state = get_pulseaudio_state()?;

    let enable_fading = state.unmuted_inputs.len() > 0;
    
    // Create a guard that will automatically restore audio state when dropped
    let mut guard = AudioStateGuard::new(state);
    
    // Fade out if needed and still running
    if enable_fading && fade_out > 0.0 && running.load(Ordering::SeqCst) {
        let fade_out_steps = 10;
        let fade_out_step_duration = Duration::from_secs_f32(fade_out / fade_out_steps as f32);
        
        for step in 0..fade_out_steps {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            
            let volume_factor = 1.0 - (step as f32 / fade_out_steps as f32);
            let step_volume = (guard.current_volume as f32 * volume_factor) as u8;
            run_command("pactl", &["set-sink-volume", &guard.default_sink, &format!("{}%", step_volume)])?;
            thread::sleep(fade_out_step_duration);
        }
    }
    
    // If we're still running, continue with notification
    if running.load(Ordering::SeqCst) {
        // Mute all unmuted sink inputs
        for input in &guard.unmuted_inputs {
            run_command("pactl", &["set-sink-input-mute", input, "1"])?;
        }
        
        // Set volume for notification
        run_command("pactl", &["set-sink-volume", &guard.default_sink, &format!("{}%", volume)])?;
        
        // Play the notification sound
        let sound_path_str = sound_path.to_string_lossy();
        run_command("paplay", &[&sound_path_str])?;
        
        // Fade in if needed and still running
        if enable_fading && fade_in > 0.0 && running.load(Ordering::SeqCst) {
            // Unmute all previously unmuted inputs
            for input in &guard.unmuted_inputs {
                run_command("pactl", &["set-sink-input-mute", input, "0"])?;
            }
            
            let fade_in_steps = 10;
            let fade_in_step_duration = Duration::from_secs_f32(fade_in / fade_in_steps as f32);
            
            for step in 0..fade_in_steps {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                
                let volume_factor = step as f32 / fade_in_steps as f32;
                let step_volume = (guard.current_volume as f32 * volume_factor) as u8;
                run_command("pactl", &["set-sink-volume", &guard.default_sink, &format!("{}%", step_volume)])?;
                thread::sleep(fade_in_step_duration);
            }
        } else {
            // If no fade-in or interrupted, just restore everything
            guard.cleanup()?;
        }
    }
    
    // The guard will automatically call cleanup when it goes out of scope
    // This ensures cleanup happens even if there's an error or interruption
    
    Ok(())
}

fn print_help_info() {
    println!("VH Notification Sound");
    println!("A simple application that plays notification sounds while temporarily fading out any currently playing audio.");
    println!("This application is designed specifically for Linux systems with PulseAudio.");
    println!();
    println!("GitHub Repository: https://github.com/vhqtvn/vh-notification-sound");
    println!();
    println!("USAGE:");
    println!("  vh-notification-sound [OPTIONS] <SOUND>");
    println!();
    println!("ARGS:");
    println!("  <SOUND>  Sound alias from config or path to audio file");
    println!();
    println!("OPTIONS:");
    println!("  -f, --fade-out <SECONDS>   Fade out duration in seconds [default: 0.3]");
    println!("  -i, --fade-in <SECONDS>    Fade in duration in seconds [default: 0.3]");
    println!("  -v, --volume <PERCENT>     Output volume percentage (0-100) [default: 75]");
    println!("  -c, --config <FILE>        Path to config file");
    println!("  -l, --list-sounds          List available sound aliases from config");
    println!("  -h, --help-info            Show this help information");
    println!("  -d, --detach               Detach process and run in background");
    println!("      --help                 Show the automatically generated help message");
    println!();
    println!("ENVIRONMENT VARIABLES:");
    println!("  VH_NOTIFICATION_FADE_OUT   Default fade-out duration in seconds");
    println!("  VH_NOTIFICATION_FADE_IN    Default fade-in duration in seconds");
    println!("  VH_NOTIFICATION_VOLUME     Default output volume percentage (0-100)");
    println!("  VH_NOTIFICATION_CONFIG     Path to the configuration file");
    println!("  VH_NOTIFICATION_DETACH     Detach process and run in background");
    println!();
    println!("EXAMPLES:");
    println!("  vh-notification-sound default");
    println!("  vh-notification-sound --fade-out 0.5 --fade-in 0.2 --volume 80 /path/to/sound.mp3");
    println!("  vh-notification-sound -d default");
    println!("  vh-notification-sound -l");
}

fn print_sound_aliases(config: &Config) {
    if config.sounds.is_empty() {
        println!("No sound aliases found in config.");
        println!("You can define sound aliases in your config file (~/.config/vh-notification-sound.yml).");
        return;
    }
    
    println!("Available sound aliases:");
    for (alias, path) in &config.sounds {
        println!("  {}: {}", alias, path);
    }
}

fn acquire_lock(lock_path: &PathBuf) -> Result<Option<File>> {
    // Check if lock file exists and is valid
    if lock_path.exists() {
        let mut file = File::open(lock_path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        
        // Check if the process in the lock file is still running
        if let Ok(pid) = contents.trim().parse::<i32>() {
            let proc_path = PathBuf::from(format!("/proc/{}", pid));
            if proc_path.exists() {
                return Err(anyhow::anyhow!("Another notification is currently playing (PID: {}).", pid));
            }
        }
        
        // If the process is not running, remove the stale lock
        std::fs::remove_file(lock_path)?;
    }
    
    // Create new lock file
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(lock_path)?;
    
    // Write current PID to lock file
    let pid = std::process::id().to_string();
    let mut lock_file = file;
    lock_file.write_all(pid.as_bytes())?;
    lock_file.flush()?;
    
    Ok(Some(lock_file))
}
