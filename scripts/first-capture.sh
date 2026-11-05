#!/bin/sh
# Keep the capture process interface beside the uploaded Rust executable.
exec ./udpstp-bench capture "$@"
