import type { ImageAttachment } from "./types.ts";

/** At most this many images ride on the motion. Storage, prompts and the engine agree. */
export const MAX_IMAGES = 6;

/**
 * Shrink a picked image to something a context window can actually carry:
 * max 1568px on a side, JPEG. Full-resolution photos bill thousands of image
 * tokens a turn and blow past localStorage on save; a downscaled one argues
 * just as well about charts, screenshots and diagrams.
 */
export function downscaleImage(file: File): Promise<ImageAttachment> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      const MAX_DIM = 1568;
      const scale = Math.min(1, MAX_DIM / Math.max(img.width, img.height));
      const w = Math.max(1, Math.round(img.width * scale));
      const h = Math.max(1, Math.round(img.height * scale));
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        reject(new Error("this browser cannot process images"));
        return;
      }
      ctx.drawImage(img, 0, 0, w, h);
      canvas.toBlob(
        (blob) => {
          if (!blob) {
            reject(new Error(`"${file.name}" could not be read`));
            return;
          }
          const reader = new FileReader();
          reader.onload = () =>
            resolve({
              id: `img-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`,
              name: file.name.slice(0, 80) || "image",
              mediaType: "image/jpeg",
              dataUrl: String(reader.result),
            });
          reader.onerror = () => reject(new Error(`"${file.name}" could not be read`));
          reader.readAsDataURL(blob);
        },
        "image/jpeg",
        0.85,
      );
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error(`"${file.name}" is not a readable image`));
    };
    img.src = url;
  });
}
