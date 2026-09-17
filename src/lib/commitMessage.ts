export const MAX_COMMIT_MESSAGE_BYTES = 64 * 1024;

export function utf8ByteLength(message: string): number {
  return new TextEncoder().encode(message).byteLength;
}

export function isCommitMessageEligible(message: string): boolean {
  return message.trim().length > 0 && !message.includes("\0") && utf8ByteLength(message) <= MAX_COMMIT_MESSAGE_BYTES;
}

export function messageLimitReason(message: string): string | null {
  if (message.includes("\0")) return "Commit messages cannot contain a NUL character.";
  const byteLength = utf8ByteLength(message);
  if (byteLength > MAX_COMMIT_MESSAGE_BYTES) return `Commit message is ${byteLength} bytes. The limit is ${MAX_COMMIT_MESSAGE_BYTES} bytes.`;
  return null;
}
