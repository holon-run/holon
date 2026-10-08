import SwiftUI
import ImageIO

/// Rasterize PDF pages only: no annotations, actions, forms, scripts or remote loads.
actor FileRasterReader {
    private let url: URL
    init(url: URL) { self.url = url }

    func pdfPageCount() throws -> Int {
        guard let document = CGPDFDocument(url as CFURL), document.numberOfPages > 0,
              document.numberOfPages <= 10_000 else { throw FilesFailure.unsupported }
        return document.numberOfPages
    }

    func image(page: Int?) throws -> Data {
        try Task.checkCancellation()
        let image: CGImage
        if let page {
            guard let document = CGPDFDocument(url as CFURL), let pdf = document.page(at: page + 1) else {
                throw FilesFailure.unsupported
            }
            let box = pdf.getBoxRect(.cropBox)
            guard box.width.isFinite, box.height.isFinite, box.width > 0, box.height > 0 else { throw FilesFailure.unsupported }
            let scale = min(2, 3072 / max(box.width, box.height))
            let width = max(1, Int(ceil(box.width * scale))), height = max(1, Int(ceil(box.height * scale)))
            guard let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
                throw FilesFailure.unsupported
            }
            context.setFillColor(CGColor(gray: 1, alpha: 1)); context.fill(CGRect(x: 0, y: 0, width: width, height: height))
            context.concatenate(pdf.getDrawingTransform(.cropBox, rect: CGRect(x: 0, y: 0, width: width, height: height), rotate: 0, preserveAspectRatio: true))
            context.drawPDFPage(pdf)
            guard let raster = context.makeImage() else { throw FilesFailure.unsupported }; image = raster
        } else {
            guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
                  let raster = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceThumbnailMaxPixelSize: 3072,
                    kCGImageSourceCreateThumbnailWithTransform: true
                  ] as CFDictionary) else { throw FilesFailure.unsupported }
            image = raster
        }
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil) else {
            throw FilesFailure.unsupported
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { throw FilesFailure.unsupported }
        try Task.checkCancellation()
        return data as Data
    }
}

struct FileZoomImage: UIViewRepresentable {
    let image: UIImage
    func makeUIView(context: Context) -> ZoomScroll {
        let view = ZoomScroll(); view.delegate = context.coordinator
        view.minimumZoomScale = 1; view.maximumZoomScale = 8
        view.imageView.contentMode = .scaleAspectFit
        view.addSubview(view.imageView)
        view.accessibilityIdentifier = "files.raster"
        view.accessibilityLabel = String(localized: "files.zoom")
        return view
    }
    func updateUIView(_ view: ZoomScroll, context: Context) {
        if view.imageView.image !== image { view.imageView.image = image; view.setZoomScale(1, animated: false); view.setNeedsLayout() }
    }
    func makeCoordinator() -> Coordinator { Coordinator() }
    final class Coordinator: NSObject, UIScrollViewDelegate {
        func viewForZooming(in scrollView: UIScrollView) -> UIView? { (scrollView as? ZoomScroll)?.imageView }
    }
    final class ZoomScroll: UIScrollView {
        let imageView = UIImageView()
        override func layoutSubviews() {
            super.layoutSubviews()
            if zoomScale == 1 { imageView.frame = bounds; contentSize = bounds.size }
        }
    }
}
