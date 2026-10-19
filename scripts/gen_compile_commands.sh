#!/usr/bin/env bash
# Regenerate compile_commands.json for testdata/sample_project via CMake.
# Runs inside WSL. Idempotent.
set -eu

PROJ="/mnt/e/TestDeepSeek/testdata/sample_project"
cd "$PROJ"
rm -rf build
cmake -B build -G Ninja -DCMAKE_EXPORT_COMPILE_COMMANDS=ON >/dev/null
cp build/compile_commands.json ./compile_commands.json
echo "compile_commands.json regenerated: $(pwd)/compile_commands.json"
