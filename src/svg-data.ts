/**
 * Render report SVG as image data rather than injecting report content into the
 * application DOM. SVG loaded through an <img> cannot execute page scripts.
 */
export function svgImageDataUrl(svg: string): string {
  const bytes = new TextEncoder().encode(svg);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return `data:image/svg+xml;base64,${btoa(binary)}`;
}
