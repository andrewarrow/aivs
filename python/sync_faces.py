"""Conservative YuNet face tracks and the upstream SyncNet face crop geometry."""
from __future__ import annotations

from collections import deque
import cv2
import numpy as np


def iou(a, b):
    lo, hi = np.maximum(a[:2], b[:2]), np.minimum(a[:2] + a[2:4], b[:2] + b[2:4])
    overlap = float(np.prod(np.maximum(hi - lo, 0)))
    return overlap / max(float(a[2] * a[3] + b[2] * b[3]) - overlap, 1.0)


def crop(frame, box):
    x, y, width, height = box[:4]
    size = max(width, height) / 2
    cx, cy = x + width / 2, y + height / 2
    # SyncNet's published preprocessing: 40% expansion, biased toward the mouth.
    pad = int(size * 1.8) + 2
    padded = cv2.copyMakeBorder(frame, pad, pad, pad, pad, cv2.BORDER_CONSTANT, value=(110, 110, 110))
    face = padded[int(cy + pad - size):int(cy + pad + size * 1.8),
                  int(cx + pad - size * 1.4):int(cx + pad + size * 1.4)]
    return cv2.resize(face, (224, 224))


def embeddings(video, detector_path, model, torch, progress):
    """No long-gap interpolation or face identity guesses across ambiguous tracks."""
    detector = cv2.FaceDetectorYN.create(str(detector_path), "", (320, 320), 0.85, 0.3, 5000)
    capture = cv2.VideoCapture(str(video))
    if not capture.isOpened():
        raise RuntimeError("Cannot decode the dialogue review video.")
    tracks, pending = [], []
    frame_index = 0

    def flush():
        if not pending:
            return
        batch = np.stack([item[2] for item in pending]).transpose(0, 4, 1, 2, 3)
        with torch.inference_mode():
            output = model.forward_lip(torch.from_numpy(batch).float()).numpy()
        for (track, start, _), feature in zip(pending, output):
            track["features"][start] = feature
        pending.clear()

    try:
        while True:
            ok, frame = capture.read()
            if not ok:
                break
            height, width = frame.shape[:2]
            detector.setInputSize((width, height))
            _, found = detector.detect(frame)
            detections = [] if found is None else [box for box in found if min(box[2:4]) >= 40]
            used = set()
            for box in detections:
                candidates = sorted([(iou(track["box"], box), idx) for idx, track in enumerate(tracks)
                                     if track["last"] == frame_index - 1 and idx not in used], reverse=True)
                # Ambiguous association starts a fresh track instead of swapping identities.
                unique = candidates and candidates[0][0] >= 0.45 and (
                    len(candidates) == 1 or candidates[0][0] - candidates[1][0] >= 0.15)
                if unique:
                    index = candidates[0][1]
                    track = tracks[index]
                else:
                    index = len(tracks)
                    track = {"id": index + 1, "box": box, "last": -1, "crops": deque(maxlen=5),
                             "features": {}, "boxes": deque(maxlen=5)}
                    tracks.append(track)
                used.add(index)
                if track["last"] != frame_index - 1:
                    track["crops"].clear()
                    track["boxes"].clear()
                track["box"], track["last"] = box, frame_index
                # Strong profiles and hidden mouths are not adequate evidence.
                eyes = np.linalg.norm(box[4:6] - box[6:8]) / max(float(box[2]), 1)
                mouth = np.linalg.norm(box[10:12] - box[12:14]) / max(float(box[2]), 1)
                if eyes < 0.18 or mouth < 0.12:
                    track["crops"].clear()
                    track["boxes"].clear()
                    continue
                track["boxes"].append(box[:4])
                smooth = np.median(np.stack(track["boxes"]), axis=0)
                track["crops"].append(crop(frame, smooth))
                if len(track["crops"]) == 5:
                    pending.append((track, frame_index - 4, np.stack(track["crops"])))
                    if len(pending) >= 8:
                        flush()
            frame_index += 1
            if frame_index % 50 == 0:
                progress(f"Checking visible faces: {frame_index / 25:.1f}s")
        flush()
    finally:
        capture.release()
    return [track for track in tracks if track["features"]], frame_index
