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
RESOLUTION=""
AUDIO_BITRATE="192k"

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
        -r|--resolution)
            RESOLUTION="$2"
            shift 2
            ;;
        -a|--audio-bitrate)
            AUDIO_BITRATE="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [options] <input>"
            echo ""
            echo "Options:"
            echo "  -b, --bitrate <rate>       Video bitrate (default: 4M)"
            echo "  -r, --resolution <res>     Scale to resolution: 720, 480, or WxH (e.g. 1280x720)"
            echo "  -a, --audio-bitrate <rate> Audio bitrate (default: 192k)"
            echo "  -o, --output <file>        Output filename"
            echo "  -h, --help                 Show this help"
            echo ""
            echo "Examples:"
            echo "  $0 movie.mkv                           # 1080p @ 4 Mbps"
            echo "  $0 -b 2M movie.mp4                     # 1080p @ 2 Mbps"
            echo "  $0 -r 720 -b 1M movie.mp4              # 720p @ 1 Mbps"
            echo "  $0 -r 480 -b 500k -a 96k movie.mp4     # 480p @ 500 Kbps (low bandwidth)"
            echo ""
            echo "Recommended settings:"
            echo "  High quality:   -r 1080 -b 4M"
            echo "  Medium quality: -r 720 -b 1M"
            echo "  Low bandwidth:  -r 480 -b 500k -a 96k"
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
    if [ -n "$RESOLUTION" ]; then
        OUTPUT="${BASENAME}_${RESOLUTION}p_${BITRATE}.mp4"
    else
        OUTPUT="${BASENAME}_${BITRATE}.mp4"
    fi
fi

echo "Transcoding: $INPUT"
echo "Output:      $OUTPUT"
echo "Bitrate:     $BITRATE"
echo ""

# Build video filter chain
VF_PARTS=()

# Add scaling if requested
if [ -n "$RESOLUTION" ]; then
    case "$RESOLUTION" in
        1080) VF_PARTS+=("scale=1920:-2") ;;
        720)  VF_PARTS+=("scale=1280:-2") ;;
        480)  VF_PARTS+=("scale=854:-2") ;;
        *x*)  VF_PARTS+=("scale=$RESOLUTION") ;;
        *)    VF_PARTS+=("scale=${RESOLUTION}:-2") ;;
    esac
    echo "Scaling to: $RESOLUTION"
fi

# Check if source is HDR (10-bit or has HDR color transfer)
PIX_FMT=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt -of csv=p=0 "$INPUT" 2>/dev/null)
COLOR_TRC=$(ffprobe -v error -select_streams v:0 -show_entries stream=color_transfer -of csv=p=0 "$INPUT" 2>/dev/null)

if [[ "$PIX_FMT" == *"10"* ]] || [[ "$COLOR_TRC" == "smpte2084" ]] || [[ "$COLOR_TRC" == "arib-std-b67" ]]; then
    echo "Detected HDR/10-bit content, applying tone mapping..."
    if ffmpeg -filters 2>&1 | grep -q zscale; then
        VF_PARTS+=("zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p")
    else
        echo "Warning: zscale not available for HDR conversion."
        VF_PARTS+=("format=yuv420p")
    fi
else
    VF_PARTS+=("format=yuv420p")
fi

# Join filters with commas
VF=$(IFS=','; echo "${VF_PARTS[*]}")

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
    -b:a "$AUDIO_BITRATE" \
    -ac 2 \
    -movflags +faststart \
    "$OUTPUT"

echo ""
echo "Done! Output: $OUTPUT"
echo ""
echo "Play with:"
echo "  ./target/release/foundry-player \"$OUTPUT\" --shared"
