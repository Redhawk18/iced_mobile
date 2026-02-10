#!/bin/bash

# This script copies the correct Rust library based on the SDK being built for.
# Add this as a "Run Script" build phase in Xcode BEFORE "Link Binary With Libraries".

set -e

CONFIG="${CONFIGURATION:-Release}"
LIB_DIR="$PROJECT_DIR/../target/ios/$CONFIG"

if [[ "$PLATFORM_NAME" == "iphonesimulator" ]]; then
    echo "Copying simulator library..."
    cp "$LIB_DIR/simulator/libmobile.a" "$LIB_DIR/libmobile.a"
else
    echo "Copying device library..."
    cp "$LIB_DIR/device/libmobile.a" "$LIB_DIR/libmobile.a"
fi

echo "Rust library ready for linking ($PLATFORM_NAME, $CONFIG)"
