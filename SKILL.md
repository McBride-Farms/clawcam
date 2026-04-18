---
name: clawcam
description: AI-powered camera monitoring for Raspberry Pi — YOLOv8 detection with any camera (Pi Camera Module, USB webcam, conference cam), structured predictions, 1080p snapshots, webhooks, TTS, and mic capture.
metadata: {"clawdbot":{"emoji":"📷","requires":{"bins":["clawcam"]}}}
---

# clawcam Skill

A specialized tool for managing AI-powered camera devices on Raspberry Pi via `clawcam`. This skill allows for registering devices, deploying the on-device YOLO detection monitor with webhook push, capturing snapshots or video clips, speaking through the device speaker, and listening via the device microphone.

## Overview

`clawcam` SSHes into your Raspberry Pi, deploys a monitor binary with a YOLOv8n ONNX model, and pushes detection events directly to your webhook endpoint. GStreamer handles camera capture from any connected source — Pi Camera Module, USB webcam, or conference camera. Each webhook includes a 1080p snapshot and structured AI predictions (class, confidence score, bounding box).

## Requirements

- **Hardware:** Raspberry Pi (3B+/4/5) with any supported camera:
  - Pi Camera Module (via libcamera)
  - USB webcam (via V4L2)
  - USB conference camera (via V4L2)
- **Network:** SSH key access to `pi@<device_ip>` (or custom user).
- **Host Software:** `clawcam` binary installed and available in the system PATH.
- **Host Software:** Cross-compiled `clawcam` binary for the target Pi architecture (aarch64 or armv7).
- **Host Software:** YOLOv8n ONNX model at `models/yolov8n.onnx`.

## Usage

### 0. Register a Device
Add a Pi camera to the device registry before using any other commands.
```bash
clawcam device add barn-cam 192.168.1.50
clawcam device add garage-cam 192.168.1.51 --port 2222 --user admin
clawcam device list
clawcam device remove barn-cam
```

### 1. Deploy with Webhook
Deploy the on-device monitor that pushes detection events directly to your endpoint.
```bash
clawcam setup barn-cam \
  --webhook http://your-host:8080/hooks/clawcam \
  --webhook-token YOUR_TOKEN
```

Setup auto-detects the connected camera (libcamera or V4L2), installs GStreamer and dependencies, uploads the binary and YOLO model, and creates a systemd service for boot persistence.

### 2. Check Device Status
Monitor the health of the detection pipeline, camera, model, and system resources.
```bash
clawcam status barn-cam
```
*Note: Add `--json` for machine-readable output.*

### 3. Speak Through Device
Send a text-to-speech message through the device's speaker.
```bash
clawcam speak barn-cam "Hello, this is a security notice"
```
*Optional Flags:*
- `--volume <1-100>`: Speaker volume (default 80).

### 4. Listen to Device Mic
Capture audio from the device microphone.
```bash
clawcam listen barn-cam --dur 10 --out recording.wav
```

### 5. Capture Media
Capture snapshots or clips from the camera.

**Snapshot:**
```bash
clawcam snap barn-cam --out shot.jpg
```

**Clip:**
```bash
clawcam clip barn-cam --dur 10 --out clip.mp4
```

### 6. Teardown
Stop the on-device monitor and clean up.
```bash
clawcam teardown barn-cam
```

## Webhook Payload

When a detection occurs, the on-device monitor runs YOLO inference and POSTs:

```json
{
  "ts": "Apr 17 14:30:45",
  "epoch": 1776437445,
  "type": "motion",
  "detail": "ai_detected",
  "source": "clawcam",
  "host": "192.168.1.50",
  "image": "<base64 1080p JPEG>",
  "predictions": [
    {
      "class": "person",
      "class_id": 0,
      "score": 0.87,
      "left": 120,
      "top": 80,
      "right": 320,
      "bottom": 430
    }
  ]
}
```

## Detection Classes

Uses the full COCO 80-class set via YOLOv8n. Key classes for monitoring:

| Class | ID |
|-------|----|
| person | 0 |
| bicycle | 1 |
| car | 2 |
| motorcycle | 3 |
| bus | 5 |
| truck | 7 |
| bird | 14 |
| cat | 15 |
| dog | 16 |
| backpack | 24 |
| suitcase | 28 |

Default confidence threshold: 0.4. Default IOU threshold: 0.45.

## Environment Variables

The on-device monitor reads:

| Variable | Description | Default |
|----------|-------------|---------|
| `CLAWCAM_CAMERA_SOURCE` | GStreamer source element | `v4l2src` |
| `CLAWCAM_MODEL_PATH` | Path to ONNX model | `/usr/local/share/clawcam/yolov8n.onnx` |

## Troubleshooting

- **No camera detected:** Ensure a Pi Camera Module or USB camera is connected. Check with `ls /dev/video*` or `libcamera-hello --list-cameras`.
- **SSH failures:** Verify your SSH keys are authorized on the Pi (`ssh-copy-id pi@<ip>`).
- **Service not starting:** Check logs with `journalctl -u clawcam` on the Pi.
- **Low detection accuracy:** Try a larger YOLO model (yolov8s.onnx) at the cost of inference speed.
- **Slow inference:** YOLOv8n runs at ~2 FPS on Pi 4. Consider reducing resolution or inference interval.
- **Empty predictions:** If `predictions` is empty, nothing exceeded the 0.4 confidence threshold in the frame.
