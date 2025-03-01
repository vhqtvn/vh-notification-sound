# VH Notification Sound

A simple Rust application that plays notification sounds while temporarily fading out any currently playing audio. This application is designed specifically for Linux systems with PulseAudio.

## Features

- Plays notification sounds with configurable fade-out and fade-in effects
- Temporarily reduces the volume of all currently playing audio
- Configurable output volume for notification sounds
- Supports configuration via command-line arguments, environment variables, and config files
- Allows sound aliases for easy reference to commonly used sounds
- Built-in help and sound alias listing commands

## Installation

### One-line Installation (Linux)

You can install the latest version with a single command:

```bash
curl -sSL https://raw.githubusercontent.com/vhqtvn/vh-notification-sound/main/install.sh | sudo bash
```

This will download and install the latest release to `/usr/local/bin`. You can customize the installation directory:

```bash
curl -sSL https://raw.githubusercontent.com/vhqtvn/vh-notification-sound/main/install.sh | sudo bash -s -- --dir=$HOME/.local/bin
```

### From Releases

You can download pre-built binaries from the [Releases](https://github.com/vhqtvn/vh-notification-sound/releases) page.

### From Source

```bash
# Clone the repository
git clone https://github.com/vhqtvn/vh-notification-sound.git
cd vh-notification-sound

# Build the application
cargo build --release

# Copy the binary to a location in your PATH
cp target/release/vh-notification-sound ~/.local/bin/
```

## Dependencies

This application requires PulseAudio to be installed on your system. It is designed to work exclusively on Linux systems with PulseAudio as the audio server.

- PulseAudio (`libpulse0`)
- PulseAudio utilities (`pulseaudio-utils`)

Install dependencies:

```bash
# On Debian/Ubuntu
sudo apt-get install libpulse0 pulseaudio-utils

# On Fedora/RHEL
sudo dnf install pulseaudio-libs pulseaudio-utils

# On Arch Linux
sudo pacman -S libpulse pulseaudio-utils
```

## Usage

```bash
# Play a sound file
vh-notification-sound /path/to/sound.mp3

# Play a sound from your home directory using tilde expansion
vh-notification-sound ~/sounds/notification.mp3

# Play a sound using an alias defined in the config
vh-notification-sound default

# Specify custom fade durations and volume
vh-notification-sound --fade-out 0.5 --fade-in 0.2 --volume 80 /path/to/sound.mp3

# List available sound aliases from your config
vh-notification-sound --list-sounds

# Show help information
vh-notification-sound --help-info
```

## Configuration

The application can be configured using a YAML configuration file. The file can be specified using the `--config` option or placed in one of the following locations:

- `./vh-notification-sound.yml` (current directory)
- `~/.config/vh-notification-sound.yml`
- `~/.vh-notification-sound.yml`

Example configuration file:

```yaml
# Default fade durations in seconds
fade_out: 0.5
fade_in: 0.3

# Output volume percentage for notification sound (0-100)
volume: 75

# Sound aliases
sounds:
  default: /usr/share/sounds/freedesktop/stereo/message.oga
  error: /usr/share/sounds/freedesktop/stereo/dialog-error.oga
  warning: /usr/share/sounds/freedesktop/stereo/dialog-warning.oga
  complete: /usr/share/sounds/freedesktop/stereo/complete.oga
  bell: /usr/share/sounds/freedesktop/stereo/bell.oga
  custom: ~/sounds/my-notification.mp3
```

> **Note**: Sound paths support tilde (~) expansion, so you can use `~/path/to/sound.mp3` to reference files in your home directory.

## Environment Variables

- `VH_NOTIFICATION_FADE_OUT`: Default fade-out duration in seconds
- `VH_NOTIFICATION_FADE_IN`: Default fade-in duration in seconds
- `VH_NOTIFICATION_VOLUME`: Default output volume percentage (0-100)
- `VH_NOTIFICATION_CONFIG`: Path to the configuration file

## License

MIT