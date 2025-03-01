use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Read,
    os::fd::IntoRawFd,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
// Import specific items from libc instead of the entire module
use libc::{close, dup2, fork, setsid, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};

// Define notification states for state tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum NotificationState {
    FadingOut,
    Playing,
    FadingIn,
    Idle,
}

// Common constant for fade steps
const FADE_STEPS: u8 = 10;

// Lock file information including notification state
#[derive(Debug, Serialize, Deserialize)]
struct LockInfo {
    pid: u32,
    state: NotificationState,
    // Used for IPC to request new notifications
    new_request: Option<String>,
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "A simple application that plays notification sounds while temporarily fading out any currently playing audio. Designed for Linux systems with PulseAudio.",
    long_about = "A simple application that plays notification sounds while temporarily fading out any currently playing audio.\nDesigned specifically for Linux systems with PulseAudio.\n\nGitHub Repository: https://github.com/vhqtvn/vh-notification-sound"
)]
struct Args {
    /// Sound alias or path to audio file
    #[arg(index = 1)]
    sound: Option<String>,

    /// Fade out duration in seconds
    #[arg(short, long, env = "VH_NOTIFICATION_FADE_OUT")]
    fade_out: Option<f32>,

    /// Fade in duration in seconds
    #[arg(short, long, env = "VH_NOTIFICATION_FADE_IN")]
    fade_in: Option<f32>,

    /// Output volume percentage for notification sound (0-100)
    #[arg(short, long, env = "VH_NOTIFICATION_VOLUME")]
    volume: Option<u8>,

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
    // Current fade state (0 = fully faded out, FADE_STEPS = full volume)
    fade_state: u8,
}

impl AudioStateGuard {
    fn new(state: PulseAudioState) -> Self {
        Self {
            default_sink: state.default_sink,
            current_volume: state.current_volume,
            unmuted_inputs: state.unmuted_inputs,
            cleaned_up: false,
            fade_state: FADE_STEPS, // Start at full volume
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

// Add this struct before the play_notification function
struct NotificationContext<'a> {
    sound_path: PathBuf,
    fade_out: f32,
    fade_in: f32,
    volume: u8,
    running: &'a Arc<AtomicBool>,
    lock_path: &'a PathBuf,
    notification_queue: &'a Arc<Mutex<Vec<PathBuf>>>,
    guard: &'a mut AudioStateGuard,
    enable_fading: bool,
    audio_already_prepared: bool,
}

fn main() -> Result<()> {
    // Parse all arguments
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

    // Determine parameters with proper precedence: command line > environment > config > defaults
    let fade_out = args
        .fade_out
        .or_else(|| {
            std::env::var("VH_NOTIFICATION_FADE_OUT")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .or(config.fade_out)
        .unwrap_or(0.3);

    let fade_in = args
        .fade_in
        .or_else(|| {
            std::env::var("VH_NOTIFICATION_FADE_IN")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .or(config.fade_in)
        .unwrap_or(0.3);

    let volume = args
        .volume
        .or_else(|| {
            std::env::var("VH_NOTIFICATION_VOLUME")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .or(config.volume)
        .unwrap_or(75)
        .min(100);

    // Resolve sound path (check if it's an alias in config)
    let sound_path = resolve_sound_path(&sound, &config)?;

    // If detach is enabled, fork the process
    if args.detach {
        match unsafe { fork() } {
            -1 => {
                return Err(anyhow::anyhow!("Failed to fork process"));
            }
            0 => {
                // Child process continues
                // Redirect standard file descriptors to /dev/null
                let null_fd = std::fs::File::open("/dev/null")?.into_raw_fd();
                unsafe {
                    dup2(null_fd, STDIN_FILENO);
                    dup2(null_fd, STDOUT_FILENO);
                    dup2(null_fd, STDERR_FILENO);
                    close(null_fd);
                }

                // Create a new session
                if unsafe { setsid() } < 0 {
                    std::process::exit(1);
                }
            }
            _ => {
                // Parent process exits
                return Ok(());
            }
        }
    }

    // Set up signal handling for clean shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        eprintln!("Received interrupt signal, cleaning up...");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    // Determine lock file path
    let lock_path = dirs::runtime_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("vh-notification-sound.lock");

    // Try to acquire lock or send request to existing server
    match acquire_lock(&lock_path, &sound_path.to_string_lossy()) {
        Ok(None) => {
            // No existing notification server, start a new one
            run_notification_server(sound_path, fade_out, fade_in, volume, running, lock_path)?;
        }
        Ok(Some(_)) => {
            // Successfully communicated with existing process
            eprintln!("Notification request sent to running instance.");
        }
        Err(e) => {
            eprintln!("Error communicating with notification server: {}", e);
        }
    }

    Ok(())
}

fn run_notification_server(
    initial_sound: PathBuf,
    fade_out: f32,
    fade_in: f32,
    volume: u8,
    running: Arc<AtomicBool>,
    lock_path: PathBuf,
) -> Result<()> {
    // Notification queue
    let notification_queue = Arc::new(Mutex::new(vec![initial_sound]));

    // Initialize the lock file with our PID and initial state
    let lock_info = LockInfo {
        pid: std::process::id(),
        state: NotificationState::Idle,
        new_request: None,
    };

    update_lock_file(&lock_path, &lock_info)?;

    // Create a thread to check for new notification requests
    let lock_path_clone = lock_path.clone();
    let running_clone = running.clone();
    let queue_clone = notification_queue.clone();

    thread::spawn(move || {
        let check_interval = Duration::from_millis(10);
        while running_clone.load(Ordering::SeqCst) {
            // Check for new notification requests in the lock file
            if let Ok(lock_info) = read_lock_file(&lock_path_clone) {
                if let Some(new_sound_path) = lock_info.new_request {
                    // Add new sound to queue
                    let mut queue = queue_clone.lock().unwrap();
                    queue.push(PathBuf::from(&new_sound_path));

                    // Clear the request from the lock file
                    if let Ok(mut updated_info) = read_lock_file(&lock_path_clone) {
                        updated_info.new_request = None;
                        let _ = update_lock_file(&lock_path_clone, &updated_info);
                    }
                }
            }
            thread::sleep(check_interval);
        }
    });

    // Get initial PulseAudio state once for the entire server
    let state = get_pulseaudio_state()?;
    let mut guard = AudioStateGuard::new(state);
    let enable_fading = !guard.unmuted_inputs.is_empty();

    // Track whether audio is already prepared for notifications
    // Audio is considered prepared when fade_state is close to 0 (faded out)
    let mut audio_already_prepared = false;

    // Main notification playback loop
    while running.load(Ordering::SeqCst) {
        // Get next notification from queue
        let sound_to_play = {
            let mut queue = notification_queue.lock().unwrap();
            if let Some(sound) = queue.pop() {
                queue.clear();
                sound
            } else {
                break; // No more notifications to play, exit loop
            }
        };

        // Update lock file state
        if let Ok(mut lock_info) = read_lock_file(&lock_path) {
            lock_info.state = NotificationState::Idle;
            update_lock_file(&lock_path, &lock_info)?;
        }

        // Play the notification sound
        let ctx = &mut NotificationContext {
            sound_path: sound_to_play,
            fade_out,
            fade_in,
            volume,
            running: &running,
            lock_path: &lock_path,
            notification_queue: &notification_queue,
            guard: &mut guard,
            enable_fading,
            audio_already_prepared,
        };

        let (completed, interrupted) = play_notification(ctx)?;

        // Update the audio preparation state for the next notification
        if interrupted {
            // If this notification was interrupted, audio is already prepared for the next one
            // Audio is considered prepared when fade_state is close to 0 (faded out)
            audio_already_prepared = guard.fade_state < FADE_STEPS / 2;
        } else if completed {
            // If the notification played completely with fade-in, audio should be restored
            // Audio is considered not prepared when fade_state is close to FADE_STEPS (full volume)
            audio_already_prepared = false;
        }

        // Check if we're done with all notifications
        let no_more_notifications = notification_queue.lock().unwrap().is_empty();

        // If we're done (or shutting down) and audio was not fully restored, do it now
        if (no_more_notifications || !running.load(Ordering::SeqCst)) && (interrupted || !completed)
        {
            // Ensure audio state is fully restored
            guard.cleanup()?;
            audio_already_prepared = false;
            guard.fade_state = FADE_STEPS; // Reset fade state to full volume
        }
    }

    // Ensure audio state is fully restored before exiting
    guard.cleanup()?;
    guard.fade_state = FADE_STEPS; // Reset fade state to full volume

    // Clean up lock file before exiting
    let _ = std::fs::remove_file(&lock_path);

    Ok(())
}

// Refactored play_notification function
fn play_notification(ctx: &mut NotificationContext) -> Result<(bool, bool)> {
    // Track whether playback was interrupted
    let mut _was_interrupted = false;

    // Only prepare audio (fade out and mute) if it's not already prepared
    if !ctx.audio_already_prepared {
        // Update lock file state to FadingOut
        if let Ok(mut lock_info) = read_lock_file(ctx.lock_path) {
            lock_info.state = NotificationState::FadingOut;
            update_lock_file(ctx.lock_path, &lock_info)?;
        }

        // Fade out if needed and we have active audio streams
        if ctx.enable_fading && ctx.fade_out > 0.0 && ctx.running.load(Ordering::SeqCst) {
            fade_audio_out(ctx.guard, ctx.fade_out, ctx.running)?;
        } else {
            // If we're skipping the fade out, set fade_state to 0 (fully faded out)
            ctx.guard.fade_state = 0;
        }

        // Check if we should continue (user might have interrupted)
        if !ctx.running.load(Ordering::SeqCst) {
            return Ok((false, false));
        }

        // Mute all unmuted sink inputs
        for input in &ctx.guard.unmuted_inputs {
            run_command("pactl", &["set-sink-input-mute", input, "1"])?;
        }

        // Set volume for notification
        run_command(
            "pactl",
            &[
                "set-sink-volume",
                &ctx.guard.default_sink,
                &format!("{}%", ctx.volume),
            ],
        )?;
    }

    // Update lock file state to Playing
    if let Ok(mut lock_info) = read_lock_file(ctx.lock_path) {
        lock_info.state = NotificationState::Playing;
        update_lock_file(ctx.lock_path, &lock_info)?;
    }

    // Play the notification sound
    let sound_path_str = ctx.sound_path.to_string_lossy().to_string();
    let should_interrupt = Arc::new(AtomicBool::new(false));
    let should_interrupt_clone = should_interrupt.clone();

    // Thread to check if a new notification arrived while playing
    let notification_queue_clone = ctx.notification_queue.clone();
    let running_clone = ctx.running.clone();
    let sound_path_str_clone = sound_path_str.clone();
    let play_running = Arc::new(AtomicBool::new(true));
    let play_running_clone = play_running.clone();

    let monitor_thread = thread::spawn(move || {
        let check_interval = Duration::from_millis(50);
        let start_time = std::time::Instant::now();

        while running_clone.load(Ordering::SeqCst) && play_running_clone.load(Ordering::SeqCst) {
            // If queue has new items (beyond what we're currently playing)
            if notification_queue_clone.lock().unwrap().len() > 0 {
                // Signal to interrupt current playback
                should_interrupt_clone.store(true, Ordering::SeqCst);

                // Try to kill paplay
                let _ = run_command(
                    "pkill",
                    &["-f", &format!("paplay.*{}", sound_path_str_clone)],
                );
                break;
            }

            thread::sleep(check_interval);

            // Safety timeout (10 seconds) to avoid hanging if something goes wrong
            if start_time.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
    });

    // Play the sound in the main thread (we'll interrupt if needed)
    let _play_result = run_command("paplay", &[&sound_path_str]);
    play_running.store(false, Ordering::SeqCst);
    // Wait for the monitor thread to finish
    let _ = monitor_thread.join();

    // Check if we were interrupted or have a new notification waiting
    if should_interrupt.load(Ordering::SeqCst) || !ctx.notification_queue.lock().unwrap().is_empty()
    {
        _was_interrupted = true;
        // Keep fade_state as is - we're already faded out
        // Skip fade-in if interrupted or new notification waiting
        return Ok((false, true));
    }

    // Check if we should continue with fade-in
    if !ctx.running.load(Ordering::SeqCst) {
        return Ok((false, false));
    }

    // Update lock file state to FadingIn
    if let Ok(mut lock_info) = read_lock_file(ctx.lock_path) {
        lock_info.state = NotificationState::FadingIn;
        update_lock_file(ctx.lock_path, &lock_info)?;
    }

    // Unmute all previously unmuted inputs
    for input in &ctx.guard.unmuted_inputs {
        run_command("pactl", &["set-sink-input-mute", input, "0"])?;
    }

    // Fade in if needed
    if ctx.enable_fading && ctx.fade_in > 0.0 && ctx.running.load(Ordering::SeqCst) {
        fade_audio_in(ctx.guard, ctx.fade_in, ctx.running, ctx.notification_queue)?;

        // Check again after fade-in if we were interrupted
        if !ctx.notification_queue.lock().unwrap().is_empty() {
            _was_interrupted = true;
            // fade_state is already updated in fade_audio_in
            return Ok((false, true));
        }
    } else {
        // If we skipped fade-in, make sure volume is restored and fade_state is reset
        run_command(
            "pactl",
            &[
                "set-sink-volume",
                &ctx.guard.default_sink,
                &format!("{}%", ctx.guard.current_volume),
            ],
        )?;
        ctx.guard.fade_state = FADE_STEPS; // Fully faded in
    }

    // Update lock file state to Idle
    if let Ok(mut lock_info) = read_lock_file(ctx.lock_path) {
        lock_info.state = NotificationState::Idle;
        update_lock_file(ctx.lock_path, &lock_info)?;
    }

    // Return completion status: (completed successfully, was interrupted)
    Ok((true, false))
}

fn fade_audio_out(
    guard: &mut AudioStateGuard,
    fade_out: f32,
    running: &Arc<AtomicBool>,
) -> Result<()> {
    // Use the existing fade_state as the starting point
    let start_step = guard.fade_state;
    let fade_out_step_duration = Duration::from_secs_f32(fade_out / FADE_STEPS as f32);

    // Starting from current fade_state and going down to 0
    for step in (0..start_step).rev() {
        if !running.load(Ordering::SeqCst) {
            // Remember the current fade state before exiting
            guard.fade_state = step + 1;
            break;
        }

        let volume_factor = step as f32 / FADE_STEPS as f32;
        let step_volume = (guard.current_volume as f32 * volume_factor) as u8;

        run_command(
            "pactl",
            &[
                "set-sink-volume",
                &guard.default_sink,
                &format!("{}%", step_volume),
            ],
        )?;

        // Update the fade state after each step
        guard.fade_state = step;

        thread::sleep(fade_out_step_duration);
    }

    Ok(())
}

fn fade_audio_in(
    guard: &mut AudioStateGuard,
    fade_in: f32,
    running: &Arc<AtomicBool>,
    notification_queue: &Arc<Mutex<Vec<PathBuf>>>,
) -> Result<()> {
    // Use the existing fade_state as the starting point
    let start_step = guard.fade_state;
    let fade_in_step_duration = Duration::from_secs_f32(fade_in / FADE_STEPS as f32);

    // Starting from current fade_state and going up to FADE_STEPS
    for step in start_step..=FADE_STEPS {
        // Check for new notifications
        if !notification_queue.lock().unwrap().is_empty() {
            // New notification came in, remember current fade state
            guard.fade_state = step;
            return Ok(());
        }

        if !running.load(Ordering::SeqCst) {
            // Remember the current fade state before exiting
            guard.fade_state = step;
            break;
        }

        let volume_factor = step as f32 / FADE_STEPS as f32;
        let step_volume = (guard.current_volume as f32 * volume_factor) as u8;

        run_command(
            "pactl",
            &[
                "set-sink-volume",
                &guard.default_sink,
                &format!("{}%", step_volume),
            ],
        )?;

        // Update the fade state after each step
        guard.fade_state = step;

        thread::sleep(fade_in_step_duration);
    }

    // Final volume restoration
    run_command(
        "pactl",
        &[
            "set-sink-volume",
            &guard.default_sink,
            &format!("{}%", guard.current_volume),
        ],
    )?;

    Ok(())
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

fn update_lock_file(lock_path: &PathBuf, lock_info: &LockInfo) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(lock_path)?;

    serde_json::to_writer(&file, &lock_info)?;

    Ok(())
}

fn read_lock_file(lock_path: &PathBuf) -> Result<LockInfo> {
    let file = OpenOptions::new().read(true).open(lock_path)?;

    let lock_info: LockInfo = serde_json::from_reader(file)?;

    Ok(lock_info)
}

fn acquire_lock(lock_path: &PathBuf, sound_path: &str) -> Result<Option<File>> {
    // Check if lock file exists and is valid
    if lock_path.exists() {
        // Try to read the lock file as JSON
        match read_lock_file(lock_path) {
            Ok(lock_info) => {
                // Check if the process in the lock file is still running
                let proc_path = PathBuf::from(format!("/proc/{}", lock_info.pid));
                if proc_path.exists() {
                    // The process is still running, send a new notification request
                    let mut updated_info = lock_info;
                    updated_info.new_request = Some(sound_path.to_string());
                    update_lock_file(lock_path, &updated_info)?;
                    return Ok(Some(File::open(lock_path)?));
                } else {
                    // Process is not running, remove stale lock
                    std::fs::remove_file(lock_path)?;
                }
            }
            Err(_) => {
                // Lock file exists but isn't in our format, try to read it as plain text for backward compatibility
                let mut file = File::open(lock_path)?;
                let mut contents = String::new();
                file.read_to_string(&mut contents)?;

                // Check if the process in the lock file is still running
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    let proc_path = PathBuf::from(format!("/proc/{}", pid));
                    if proc_path.exists() {
                        return Err(anyhow::anyhow!(
                            "Another notification is currently playing (PID: {}).",
                            pid
                        ));
                    }
                }

                // If the process is not running, remove the stale lock
                std::fs::remove_file(lock_path)?;
            }
        }
    }

    // Create new lock file with initial state
    let initial_lock_info = LockInfo {
        pid: std::process::id(),
        state: NotificationState::Idle,
        new_request: None,
    };

    update_lock_file(lock_path, &initial_lock_info)?;

    // Return None to indicate we're starting a new process
    Ok(None)
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
