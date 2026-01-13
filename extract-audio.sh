#!/bin/bash
#
# Extract audio from video file for foundry-player HTTP streaming
#
# Usage: ./extract-audio.sh movie.mp4 [output.m4a]
#

set -e

if [ -z "$1" ]; then
    echo "Usage: $0 <input.mp4> [output.m4a]"
    echo ""
    echo "Extracts audio track for use with foundry-player --url"
    echo ""
    echo "Examples:"
    echo "  $0 movie.mp4                    # Creates movie.audio.m4a"
    echo "  $0 movie.mp4 audio.m4a          # Creates audio.m4a"
    exit 1
fi

INPUT="$1"

if [ ! -f "$INPUT" ]; then
    echo "Error: File not found: $INPUT"
    exit 1
fi

# Generate output filename
if [ -n "$2" ]; then
    OUTPUT="$2"
else
    BASENAME="${INPUT%.*}"
    OUTPUT="${BASENAME}.audio.m4a"
fi

echo "Extracting audio from: $INPUT"
echo "Output: $OUTPUT"
echo ""

ffmpeg -i "$INPUT" \
    -vn \
    -c:a aac \
    -b:a 192k \
    -ac 2 \
    "$OUTPUT"

echo ""
echo "Done! Audio extracted to: $OUTPUT"
echo ""
echo "Upload both files to your storage bucket, then run:"
echo "  foundry-player --url https://storage.example.com/movie.mp4 \\"
echo "                 --audio-url https://storage.example.com/movie.audio.m4a \\"
echo "                 --shared"
