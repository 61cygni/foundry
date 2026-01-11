#!/bin/bash
#
# Transcode video for foundry-player streaming
# Creates an optimized H.264/AAC MP4 at a specified bitrate
#
# Usage: ./transcode.sh [options] input.mp4
#

set -e

# Defaults
BITRATE="4M"
OUTPUT=""

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -b|--bitrate)
            BITRATE="$2"
            shift 2
            ;;
        -o|--output)
            OUTPUT="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [options] <input>"
            echo ""
            echo "Options:"
            echo "  -b, --bitrate <rate>   Video bitrate (default: 4M)"
            echo "  -o, --output <file>    Output filename (default: input_BITRATE.mp4)"
            echo "  -h, --help             Show this help"
            echo ""
            echo "Examples:"
            echo "  $0 movie.mkv                        # Convert to 4 Mbps"
            echo "  $0 -b 2M movie.mp4                  # Convert to 2 Mbps"
            echo "  $0 -b 2M -o small.mp4 movie.mp4    # Custom output name"
            echo ""
            echo "Recommended bitrates:"
            echo "  720p:  2-4M"
            echo "  1080p: 4-8M"
            echo "  4K:    15-25M"
            exit 0
            ;;
        -*)
            echo "Unknown option: $1"
            exit 1
            ;;
        *)
            INPUT="$1"
            shift
            ;;
    esac
done

if [ -z "$INPUT" ]; then
    echo "Error: No input file specified"
    echo "Run '$0 --help' for usage"
    exit 1
fi

if [ ! -f "$INPUT" ]; then
    echo "Error: File not found: $INPUT"
    exit 1
fi

# Generate output filename if not specified
if [ -z "$OUTPUT" ]; then
    BASENAME="${INPUT%.*}"
    OUTPUT="${BASENAME}_${BITRATE}.mp4"
fi

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
