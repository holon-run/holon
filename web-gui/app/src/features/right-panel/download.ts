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
