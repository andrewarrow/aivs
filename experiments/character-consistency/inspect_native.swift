import Foundation
import AVFoundation
import Vision
import CoreVideo

func rect(_ value: CGRect) -> [Double] {
    [value.origin.x, value.origin.y, value.width, value.height].map(Double.init)
}

func inspect(_ path: String) throws {
    let asset = AVURLAsset(url: URL(fileURLWithPath: path))
    guard let track = asset.tracks(withMediaType: .video).first else {
        throw NSError(domain: "native-review", code: 1, userInfo: [NSLocalizedDescriptionKey: "No video track"])
    }
    let reader = try AVAssetReader(asset: asset)
    let output = AVAssetReaderTrackOutput(
        track: track,
        outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA]
    )
    guard reader.canAdd(output) else {
        throw NSError(domain: "native-review", code: 2, userInfo: [NSLocalizedDescriptionKey: "Cannot add video reader output"])
    }
    reader.add(output)
    guard reader.startReading() else {
        throw reader.error ?? NSError(domain: "native-review", code: 3)
    }

    var frames = [[String: Any]]()
    while let sample = output.copyNextSampleBuffer() {
        try autoreleasepool {
            guard let pixels = CMSampleBufferGetImageBuffer(sample) else {
                throw NSError(domain: "native-review", code: 4, userInfo: [NSLocalizedDescriptionKey: "Missing decoded frame"])
            }
            let ocr = VNRecognizeTextRequest()
            ocr.recognitionLevel = .accurate
            ocr.usesLanguageCorrection = false
            ocr.recognitionLanguages = ["en-US"]
            ocr.usesCPUOnly = true
            let faces = VNDetectFaceLandmarksRequest()
            faces.usesCPUOnly = true
            let handler = VNImageRequestHandler(cvPixelBuffer: pixels, orientation: .up)
            try handler.perform([ocr, faces])

            let text: [[String: Any]] = (ocr.results ?? []).compactMap { item in
                guard let match = item.topCandidates(1).first else { return nil }
                return ["text": match.string, "confidence": match.confidence, "box": rect(item.boundingBox)]
            }
            let landmarks: [[String: Any]] = (faces.results ?? []).map { face in
                var value: [String: Any] = ["box": rect(face.boundingBox), "confidence": face.confidence]
                if let mouth = face.landmarks?.innerLips, mouth.pointCount > 0 {
                    let points = mouth.normalizedPoints
                    let xs = points.map { Double($0.x) }
                    let ys = points.map { Double($0.y) }
                    value["inner_lip_aspect"] = (ys.max()! - ys.min()!) / max(0.000001, xs.max()! - xs.min()!)
                }
                return value
            }
            frames.append([
                "frame": frames.count,
                "seconds": CMSampleBufferGetPresentationTimeStamp(sample).seconds,
                "text": text,
                "faces": landmarks,
            ])
        }
    }
    guard reader.status == .completed else {
        throw reader.error ?? NSError(domain: "native-review", code: 5, userInfo: [NSLocalizedDescriptionKey: "Video read did not complete"])
    }
    let result: [String: Any] = [
        "video": path,
        "ocr_revision": VNRecognizeTextRequest.currentRevision,
        "face_revision": VNDetectFaceLandmarksRequest.currentRevision,
        "frames": frames,
    ]
    let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
}

guard CommandLine.arguments.count == 2 else {
    fputs("usage: swift inspect_native.swift VIDEO\n", stderr)
    exit(2)
}
try inspect(CommandLine.arguments[1])
