#!/bin/bash

# Build release version
cargo build --release

# Install binary
sudo cp target/release/mta-cli /usr/local/bin/mta-cli

# Create data directory
sudo mkdir -p /usr/local/share/mta-cli
sudo cp -r gtfs_subway /usr/local/share/mta-cli/

# Set up environment variable (add to bashrc if not already there)
if ! grep -q "MTA_GTFS_PATH" ~/.bashrc; then
    echo 'export MTA_GTFS_PATH="/usr/local/share/mta-cli/gtfs_subway"' >> ~/.bashrc
    echo "✅ Added MTA_GTFS_PATH to ~/.bashrc"
fi

echo "✅ Installation complete!"
echo "🔄 Please run: source ~/.bashrc"
echo "🚇 Then try: mta-cli interactive"