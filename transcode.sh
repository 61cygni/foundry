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

# Check if source is HDR (10-bit or has HDR color transfer)
PIX_FMT=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt -of csv=p=0 "$INPUT" 2>/dev/null)
COLOR_TRC=$(ffprobe -v error -select_streams v:0 -show_entries stream=color_transfer -of csv=p=0 "$INPUT" 2>/dev/null)

# Determine video filter based on source format
if [[ "$PIX_FMT" == *"10"* ]] || [[ "$COLOR_TRC" == "smpte2084" ]] || [[ "$COLOR_TRC" == "arib-std-b67" ]]; then
    echo "Detected HDR/10-bit content, applying tone mapping..."
    # Check if zscale is available
    if ffmpeg -filters 2>&1 | grep -q zscale; then
        VF="zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
    else
        echo "Warning: zscale not available. Install ffmpeg with --enable-libzimg for best HDR conversion."
        VF="format=yuv420p"
    fi
else
    VF="format=yuv420p"
fi

ffmpeg -i "$INPUT" \
    -map 0:v:0 \
    -map 0:a:0? \
    -c:v libx264 \
    -b:v "$BITRATE" \
    -preset medium \
    -profile:v high \
    -level 4.1 \
    -vf "$VF" \
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
