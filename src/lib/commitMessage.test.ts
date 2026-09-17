import { describe, expect, it } from "vitest";

import { isCommitMessageEligible, MAX_COMMIT_MESSAGE_BYTES, messageLimitReason, utf8ByteLength } from "./commitMessage";

describe("commit message UI boundary", () => {
  it("accepts a nonblank message at the 64 KiB UTF-8 limit", () => {
    const message = "a".repeat(MAX_COMMIT_MESSAGE_BYTES);
    expect(utf8ByteLength(message)).toBe(MAX_COMMIT_MESSAGE_BYTES);
    expect(isCommitMessageEligible(message)).toBe(true);
  });

  it("rejects blank, NUL-containing, and multibyte over-limit messages before submission", () => {
    expect(isCommitMessageEligible(" \n\t")).toBe(false);
    expect(isCommitMessageEligible("valid\0message")).toBe(false);
    const tooLong = "😀".repeat(Math.floor(MAX_COMMIT_MESSAGE_BYTES / 4) + 1);
    expect(utf8ByteLength(tooLong)).toBeGreaterThan(MAX_COMMIT_MESSAGE_BYTES);
    expect(isCommitMessageEligible(tooLong)).toBe(false);
    expect(messageLimitReason(tooLong)).toContain("limit");
  });
});
