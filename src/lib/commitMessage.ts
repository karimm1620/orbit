export const MAX_COMMIT_MESSAGE_BYTES = 64 * 1024;

export function utf8ByteLength(message: string): number {
  return new TextEncoder().encode(message).byteLength;
}

function isRustWhitespace(character: string): boolean {
  const codePoint = character.codePointAt(0);
  return codePoint !== undefined && (
    (codePoint >= 0x09 && codePoint <= 0x0d)
    || codePoint === 0x20
    || codePoint === 0x85
    || codePoint === 0xa0
    || codePoint === 0x1680
    || (codePoint >= 0x2000 && codePoint <= 0x200a)
    || codePoint === 0x2028
    || codePoint === 0x2029
    || codePoint === 0x202f
    || codePoint === 0x205f
    || codePoint === 0x3000
  );
}

export function isCommitMessageEligible(message: string): boolean {
  return Array.from(message).some((character) => !isRustWhitespace(character))
    && !message.includes("\0")
    && utf8ByteLength(message) <= MAX_COMMIT_MESSAGE_BYTES;
}

export function messageLimitReason(message: string): string | null {
  if (message.includes("\0")) return "Commit messages cannot contain a NUL character.";
  const byteLength = utf8ByteLength(message);
  if (byteLength > MAX_COMMIT_MESSAGE_BYTES) return `Commit message is ${byteLength} bytes. The limit is ${MAX_COMMIT_MESSAGE_BYTES} bytes.`;
  return null;
}
