#!/usr/bin/env bash

set -e

# Default installation directory
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="$HOME/.config"
REPO_OWNER="vhqtvn"
REPO_NAME="vh-notification-sound"
BINARY_NAME="vh-notification-sound"

# Detect if running in local repository
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IS_LOCAL_REPO=false
# Check multiple conditions to ensure this is actually the vh-notification-sound repo
if [ -f "$SCRIPT_DIR/Cargo.toml" ] && \
   [ -d "$SCRIPT_DIR/src" ] && \
   [ -f "$SCRIPT_DIR/src/main.rs" ] && \
   grep -q "name = \"vh-notification-sound\"" "$SCRIPT_DIR/Cargo.toml" 2>/dev/null; then
    IS_LOCAL_REPO=true
fi

# ANSI color codes
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored messages
print_message() {
    local color=$1
    local message=$2
    echo -e "${color}${message}${NC}"
}

# Function to check if a command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Function to check if the script is run with sudo
check_sudo() {
    if [ "$EUID" -ne 0 ]; then
        print_message "$YELLOW" "Warning: This script is not being run with sudo. You may need sudo privileges to write to $INSTALL_DIR."
        print_message "$YELLOW" "If installation fails, try running with: sudo curl -sSL https://raw.githubusercontent.com/$REPO_OWNER/$REPO_NAME/main/install.sh | sudo bash"
        read -p "Continue anyway? (y/N) " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            print_message "$RED" "Installation aborted."
            exit 1
        fi
    fi
}

# Function to check dependencies
check_dependencies() {
    local missing_deps=()
    
    # If local repo, check for cargo instead of curl/jq/tar
    if [ "$IS_LOCAL_REPO" = true ]; then
        if ! command_exists cargo; then
            missing_deps+=("cargo")
        fi
    else
        for cmd in curl jq tar; do
            if ! command_exists "$cmd"; then
                missing_deps+=("$cmd")
            fi
        done
    fi
    
    if [ ${#missing_deps[@]} -gt 0 ]; then
        print_message "$RED" "Error: The following dependencies are missing:"
        for dep in "${missing_deps[@]}"; do
            echo "  - $dep"
        done
        
        print_message "$YELLOW" "Please install them and try again."
        if [ "$IS_LOCAL_REPO" = true ]; then
            print_message "$BLUE" "Install Rust from: https://rustup.rs/"
        else
            print_message "$BLUE" "On Debian/Ubuntu: sudo apt-get install ${missing_deps[*]}"
            print_message "$BLUE" "On Fedora/RHEL: sudo dnf install ${missing_deps[*]}"
            print_message "$BLUE" "On Arch Linux: sudo pacman -S ${missing_deps[*]}"
        fi
        exit 1
    fi
}

# Function to check PulseAudio dependencies
check_pulseaudio() {
    if ! command_exists pactl || ! command_exists paplay; then
        print_message "$YELLOW" "Warning: PulseAudio utilities (pactl, paplay) not found."
        print_message "$YELLOW" "vh-notification-sound requires PulseAudio to function properly."
        print_message "$BLUE" "On Debian/Ubuntu: sudo apt-get install libpulse0 pulseaudio-utils"
        print_message "$BLUE" "On Fedora/RHEL: sudo dnf install pulseaudio-libs pulseaudio-utils"
        print_message "$BLUE" "On Arch Linux: sudo pacman -S libpulse pulseaudio-utils"
        
        read -p "Continue installation anyway? (y/N) " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            print_message "$RED" "Installation aborted."
            exit 1
        fi
    fi
}

# Function to detect system architecture
detect_arch() {
    local arch=$(uname -m)
    case $arch in
        x86_64)
            echo "amd64"
            ;;
        aarch64|arm64)
            echo "arm64"
            ;;
        *)
            print_message "$RED" "Error: Unsupported architecture: $arch"
            print_message "$RED" "This installer only supports x86_64 and arm64 architectures."
            exit 1
            ;;
    esac
}

# Function to get the latest release URL
get_latest_release_url() {
    local arch=$1
    local api_url="https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/releases/latest"
    local asset_pattern="vh-notification-sound-linux-$arch.tar.gz"
    
    # Get the latest release info
    local release_info
    if ! release_info=$(curl -sSL "$api_url"); then
        print_message "$RED" "Error: Failed to fetch release information from GitHub."
        exit 1
    fi
    
    # Extract the download URL for the appropriate asset
    local download_url
    if ! download_url=$(echo "$release_info" | jq -r ".assets[] | select(.name | contains(\"$asset_pattern\")) | .browser_download_url"); then
        print_message "$RED" "Error: Failed to parse release information."
        exit 1
    fi
    
    if [ -z "$download_url" ]; then
        print_message "$RED" "Error: Could not find a release asset matching your architecture ($arch)."
        exit 1
    fi
    
    # Return the download URL without any additional output
    printf "%s" "$download_url"
}

# Function to build from local source
build_from_source() {
    print_message "$BLUE" "Building from local source..."
    
    # Change to script directory (repo root)
    cd "$SCRIPT_DIR"
    
    # Build the project in release mode
    if ! cargo build --release; then
        print_message "$RED" "Error: Failed to build the project."
        exit 1
    fi
    
    local binary_path="$SCRIPT_DIR/target/release/$BINARY_NAME"
    
    if [ ! -f "$binary_path" ]; then
        print_message "$RED" "Error: Built binary not found at $binary_path"
        exit 1
    fi
    
    print_message "$BLUE" "Installing to $INSTALL_DIR/$BINARY_NAME..."
    
    # Create the installation directory if it doesn't exist
    mkdir -p "$INSTALL_DIR"
    
    # Copy the binary to the installation directory
    MAYBE_SUDO=
    if [ "$IS_LOCAL_REPO" = true ] && [ "$EUID" -ne 0 ]; then
        MAYBE_SUDO="sudo"
    fi
    if ! $MAYBE_SUDO cp "$binary_path" "$INSTALL_DIR/$BINARY_NAME"; then
        print_message "$RED" "Error: Failed to copy the binary to $INSTALL_DIR."
        print_message "$YELLOW" "You may need to run this script with sudo."
        exit 1
    fi
    
    # Make the binary executable
    $MAYBE_SUDO chmod +x "$INSTALL_DIR/$BINARY_NAME"
    
    # Copy the config file if it exists
    if [ -f "$SCRIPT_DIR/vh-notification-sound.yml" ]; then
        print_message "$BLUE" "Installing config file to $CONFIG_DIR/vh-notification-sound.yml..."
        if [ -f "$CONFIG_DIR/vh-notification-sound.yml" ]; then
            print_message "$YELLOW" "Warning: $CONFIG_DIR/vh-notification-sound.yml already exists. Skipping."
        else
            mkdir -p "$CONFIG_DIR"
            cp "$SCRIPT_DIR/vh-notification-sound.yml" "$CONFIG_DIR/vh-notification-sound.yml"
        fi
    fi
    
    print_message "$GREEN" "Installation complete! The vh-notification-sound binary is now available at $INSTALL_DIR/$BINARY_NAME"
}

# Function to download and install the binary
install_binary() {
    local download_url=$1
    local temp_dir=$(mktemp -d)
    local temp_file="$temp_dir/vh-notification-sound.tar.gz"
    
    print_message "$BLUE" "Downloading from $download_url..."
    
    # Download the release
    if ! curl -sSL -o "$temp_file" "$download_url"; then
        print_message "$RED" "Error: Failed to download the release."
        rm -rf "$temp_dir"
        exit 1
    fi
    
    print_message "$BLUE" "Extracting files..."
    
    # Extract the archive
    if ! tar -xzf "$temp_file" -C "$temp_dir"; then
        print_message "$RED" "Error: Failed to extract the archive."
        rm -rf "$temp_dir"
        exit 1
    fi
    
    # Find the binary in the extracted files
    local binary_path
    binary_path=$(find "$temp_dir" -name "vh-notification-sound-linux-*" -type f -executable)
    
    if [ -z "$binary_path" ]; then
        print_message "$RED" "Error: Could not find the binary in the downloaded archive."
        rm -rf "$temp_dir"
        exit 1
    fi
    
    print_message "$BLUE" "Installing to $INSTALL_DIR/$BINARY_NAME..."
    
    # Create the installation directory if it doesn't exist
    mkdir -p "$INSTALL_DIR"
    
    # Copy the binary to the installation directory
    if ! cp "$binary_path" "$INSTALL_DIR/$BINARY_NAME"; then
        print_message "$RED" "Error: Failed to copy the binary to $INSTALL_DIR."
        print_message "$YELLOW" "You may need to run this script with sudo."
        rm -rf "$temp_dir"
        exit 1
    fi
    
    # Make the binary executable
    chmod +x "$INSTALL_DIR/$BINARY_NAME"
    
    # Copy the config file if it exists
    local config_file
    config_file=$(find "$temp_dir" -name "vh-notification-sound.yml" -type f)
    
    if [ -n "$config_file" ]; then
        print_message "$BLUE" "Installing config file to $CONFIG_DIR/vh-notification-sound.yml..."
        mkdir -p "$CONFIG_DIR"
        cp "$config_file" "$CONFIG_DIR/vh-notification-sound.yml"
    fi
    
    # Clean up
    rm -rf "$temp_dir"
    
    print_message "$GREEN" "Installation complete! The vh-notification-sound binary is now available at $INSTALL_DIR/$BINARY_NAME"
}

# Main function
main() {
    # Parse command line arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --dir=*)
                INSTALL_DIR="${1#*=}"
                shift
                ;;
            --config-dir=*)
                CONFIG_DIR="${1#*=}"
                shift
                ;;
            --help)
                echo "Usage: curl -sSL https://raw.githubusercontent.com/$REPO_OWNER/$REPO_NAME/main/install.sh | bash -s -- [OPTIONS]"
                echo "   or: ./install.sh [OPTIONS]  (when running from local repository)"
                echo ""
                echo "Options:"
                echo "  --dir=PATH        Set the installation directory (default: /usr/local/bin)"
                echo "  --config-dir=PATH Set the configuration directory (default: ~/.config)"
                echo "  --help            Show this help message"
                echo ""
                echo "When run from a local repository (detected by presence of Cargo.toml and src/),"
                echo "the script will build from source instead of downloading a pre-built binary."
                exit 0
                ;;
            *)
                print_message "$RED" "Error: Unknown option: $1"
                exit 1
                ;;
        esac
    done
    
    print_message "$BLUE" "vh-notification-sound installer"
    print_message "$BLUE" "================================"
    
    # Check if running in local repository
    if [ "$IS_LOCAL_REPO" = true ]; then
        print_message "$GREEN" "Detected local repository environment"
        print_message "$BLUE" "Building from source instead of downloading..."
    fi
    
    # Check if running with sudo
    check_sudo
    
    # Check dependencies
    check_dependencies
    
    # Check PulseAudio
    check_pulseaudio
    
    # Install based on environment
    if [ "$IS_LOCAL_REPO" = true ]; then
        # Build and install from local source
        build_from_source
    else
        # Detect architecture
        local arch
        arch=$(detect_arch)
        print_message "$BLUE" "Detected architecture: $arch"
        
        # Get the latest release URL - capture output to variable without any additional output
        print_message "$BLUE" "Fetching latest release information..."
        local download_url
        download_url=$(get_latest_release_url "$arch")
        
        # Install the binary
        install_binary "$download_url"
    fi
    
    print_message "$GREEN" "You can now run vh-notification-sound to play notification sounds!"
    print_message "$GREEN" "Example: $INSTALL_DIR/$BINARY_NAME /path/to/sound.mp3"
}

# Run the main function with all script arguments
main "$@" 