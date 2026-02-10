#!/bin/bash

# Script to select the correct Rust library based on build configuration and SDK

# Determine which library to use based on SDK
if [[ "$PLATFORM_NAME" == "iphonesimulator" ]]; then
    LIB_DIR="$PROJECT_DIR/../target/ios-simulator/${CONFIGURATION}"
else
    LIB_DIR="$PROJECT_DIR/../target/ios-device/${CONFIGURATION}"
fi

echo "Using Rust library from: $LIB_DIR"

# Export for XCode to use
echo "RUST_LIB_PATH=$LIB_DIR"
