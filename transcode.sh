#!/bin/bash
#
# Transcode video for foundry-player streaming
# Creates an optimized H.264/AAC MP4 at a specified bitrate
#
# Usage: ./transcode.sh input.mp4 [bitrate]
#   bitrate: Target video bitrate (default: 4M)
#            Examples: 2M, 4M, 8M, 1500k
#

set -e

if [ -z "$1" ]; then
    echo "Usage: $0 <input.mp4> [bitrate]"
    echo ""
    echo "Examples:"
    echo "  $0 movie.mkv           # Convert to 4 Mbps (default)"
    echo "  $0 movie.mp4 2M        # Convert to 2 Mbps"
    echo "  $0 movie.mp4 8M        # Convert to 8 Mbps (higher quality)"
    echo ""
    echo "Recommended bitrates:"
    echo "  720p:  2-4M"
    echo "  1080p: 4-8M"
    echo "  4K:    15-25M"
    exit 1
fi

INPUT="$1"
BITRATE="${2:-4M}"

if [ ! -f "$INPUT" ]; then
    echo "Error: File not found: $INPUT"
    exit 1
fi

# Generate output filename
BASENAME="${INPUT%.*}"
EXT="${INPUT##*.}"
OUTPUT="${BASENAME}_${BITRATE}.mp4"

echo "Transcoding: $INPUT"
echo "Output:      $OUTPUT"
echo "Bitrate:     $BITRATE"
echo ""

ffmpeg -i "$INPUT" \
    -c:v libx264 \
    -b:v "$BITRATE" \
    -preset medium \
    -profile:v high \
    -level 4.1 \
    -pix_fmt yuv420p \
    -c:a aac \
    -b:a 192k \
    -ac 2 \
    -movflags +faststart \
    "$OUTPUT"

echo ""
echo "Done! Output: $OUTPUT"
echo ""
echo "Play with:"
echo "  ./target/release/foundry-player \"$OUTPUT\" --shared"
