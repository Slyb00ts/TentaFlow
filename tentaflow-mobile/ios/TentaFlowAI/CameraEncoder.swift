// =============================================================================
// Plik: CameraEncoder.swift
// Opis: Kamera iPhone/iPad → enkoder H.264 (VideoToolbox) → ramki Annex-B → Rust
//       core (`tentaflow_mobile_push_camera_h264`). Trafia do TEGO SAMEGO potoku co
//       każda kamera: kafelek MSE + skrzynka klatek dla TentaVision i AI-głębi
//       (jeden strumień, wielu odbiorców). Addon `phone` rejestruje kamerę.
//
//       UWAGA: kamera to jedno urządzenie — gdy ARKit sceneDepth mapuje przez głębię
//       sprzętową, kamera jest zajęta; ten enkoder uruchamiamy na urządzeniach BEZ
//       głębi (wtedy obraz zasila kafelek + TentaVision + ścieżkę AI-głębi).
// =============================================================================

import AVFoundation
import VideoToolbox

final class CameraEncoder: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
    private let session = AVCaptureSession()
    private var compression: VTCompressionSession?
    private let queue = DispatchQueue(label: "phone-cam-encoder")

    func start() -> Bool {
        session.beginConfiguration()
        session.sessionPreset = .hd1280x720
        guard let cam = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back),
              let input = try? AVCaptureDeviceInput(device: cam),
              session.canAddInput(input) else {
            session.commitConfiguration()
            return false
        }
        session.addInput(input)
        let output = AVCaptureVideoDataOutput()
        output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
        output.setSampleBufferDelegate(self, queue: queue)
        guard session.canAddOutput(output) else { session.commitConfiguration(); return false }
        session.addOutput(output)
        session.commitConfiguration()

        var s: VTCompressionSession?
        let status = VTCompressionSessionCreate(
            allocator: nil, width: 1280, height: 720,
            codecType: kCMVideoCodecType_H264, encoderSpecification: nil,
            imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: nil, refcon: nil, compressionSessionOut: &s)
        guard status == noErr, let comp = s else { return false }
        VTSessionSetProperty(comp, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(comp, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_Baseline_AutoLevel)
        VTSessionSetProperty(comp, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: 30 as CFNumber)
        VTSessionSetProperty(comp, key: kVTCompressionPropertyKey_AverageBitRate, value: 4_000_000 as CFNumber)
        compression = comp
        session.startRunning()
        return true
    }

    func stop() {
        session.stopRunning()
        if let c = compression { VTCompressionSessionInvalidate(c) }
        compression = nil
    }

    // AVCapture → VideoToolbox encode.
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard let comp = compression, let px = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        VTCompressionSessionEncodeFrame(comp, imageBuffer: px, presentationTimeStamp: pts,
            duration: .invalid, frameProperties: nil, infoFlagsOut: nil) { [weak self] status, _, sample in
            guard status == noErr, let sample = sample else { return }
            self?.emit(sample)
        }
    }

    // Encoded sample (AVCC, length-prefixed) → Annex-B (start codes); prepend SPS/PPS
    // on keyframes so the core's h264parse/decoder + mp4mux see a self-contained stream.
    private func emit(_ sample: CMSampleBuffer) {
        let startCode: [UInt8] = [0, 0, 0, 1]
        var out = [UInt8]()
        // Keyframe? → prepend parameter sets.
        if let attach = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]],
           let first = attach.first,
           (first[kCMSampleAttachmentKey_NotSync] as? Bool) != true,
           let fmt = CMSampleBufferGetFormatDescription(sample) {
            var count = 0
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil, parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil)
            for i in 0..<count {
                var ptr: UnsafePointer<UInt8>?
                var size = 0
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, parameterSetIndex: i, parameterSetPointerOut: &ptr, parameterSetSizeOut: &size, parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil) == noErr, let p = ptr {
                    out.append(contentsOf: startCode)
                    out.append(contentsOf: UnsafeBufferPointer(start: p, count: size))
                }
            }
        }
        guard let block = CMSampleBufferGetDataBuffer(sample) else { return }
        var lenTotal = 0
        var dataPtr: UnsafeMutablePointer<Int8>?
        guard CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &lenTotal, dataPointerOut: &dataPtr) == noErr, let base = dataPtr else { return }
        // Walk AVCC: [4-byte big-endian length][NAL] ... → replace each length with a start code.
        var i = 0
        let bytes = UnsafeRawPointer(base).assumingMemoryBound(to: UInt8.self)
        while i + 4 <= lenTotal {
            let nalLen = (Int(bytes[i]) << 24) | (Int(bytes[i + 1]) << 16) | (Int(bytes[i + 2]) << 8) | Int(bytes[i + 3])
            i += 4
            if nalLen <= 0 || i + nalLen > lenTotal { break }
            out.append(contentsOf: startCode)
            out.append(contentsOf: UnsafeBufferPointer(start: bytes + i, count: nalLen))
            i += nalLen
        }
        out.withUnsafeBufferPointer { buf in
            if let p = buf.baseAddress {
                _ = tentaflow_mobile_push_camera_h264(p, Int32(buf.count))
            }
        }
    }
}
