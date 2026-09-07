export function triggerBlobDownload(
  blob: Blob,
  filename: string,
  documentRef: Pick<Document, "body" | "createElement"> = document,
  urlRef: Pick<typeof URL, "createObjectURL" | "revokeObjectURL"> = URL,
): void {
  const objectUrl = urlRef.createObjectURL(blob);
  const anchor = documentRef.createElement("a");
  anchor.href = objectUrl;
  anchor.download = filename;

  try {
    documentRef.body.appendChild(anchor);
    anchor.click();
  } finally {
    anchor.remove();
    urlRef.revokeObjectURL(objectUrl);
  }
}

/**
 * Start a browser-native download (or navigation) for a URL. The server is
 * expected to set `Content-Disposition: attachment` for download URLs, so no
 * `download` attribute is needed and the file never buffers in memory.
 */
export function triggerHrefDownload(
  url: string,
  documentRef: Pick<Document, "body" | "createElement"> = document,
): void {
  const anchor = documentRef.createElement("a");
  anchor.href = url;
  anchor.rel = "noopener";
  try {
    documentRef.body.appendChild(anchor);
    anchor.click();
  } finally {
    anchor.remove();
  }
}
