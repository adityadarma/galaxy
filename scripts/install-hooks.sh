#!/bin/sh
# Points Git at the tracked hooks in .githooks. Run once per clone.
set -e
cd "$(git rev-parse --show-toplevel)"
chmod +x .githooks/*
git config core.hooksPath .githooks
echo "Git hooks installed from .githooks"
