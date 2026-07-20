"""Proof the boundary is real: Python compresses a tool output through the Rust core over the C
ABI, then independently verifies it is lossless. No proxy, no network hop, no PyO3."""

import secondwind

print("ABI version:", secondwind.abi_version())

# A realistic `ls -la` tool output block.
block = "\n".join(
    f"-rw-r--r--  1 root  wheel  {100 + i * 37:>7} Jan  1 12:00 file-{i}.txt" for i in range(200)
)

result = secondwind.compress(block)
print("kind:", result["kind"], "| transform:", result.get("transform"))
print(
    f"bytes: {result['input_bytes']} -> {result['wire_bytes']}"
    f" ({100 * (result['input_bytes'] - result['wire_bytes']) / result['input_bytes']:.1f}% smaller)"
)

cert_hash = result["certificate"]["hash"]
print("certificate hash:", cert_hash[:24], "...")

verified = secondwind.verify(result["wire"], cert_hash)
print("independently verified lossless:", verified)
assert verified, "verification must pass for a lossless wire"

# And a tampered wire must fail verification: the proof is real, not decorative.
tampered = result["wire"].replace("file-0", "file-X", 1)
print("tampered wire verifies:", secondwind.verify(tampered, cert_hash), "(must be False)")
