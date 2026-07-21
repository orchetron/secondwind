use serde_json::Value;

pub struct Encoded {
    pub wire: String,
    pub decoded: Value,
}

// Returns the decoded value alongside the wire so the gate can verify the round trip without trusting the transform.
pub trait Transform {
    fn id(&self) -> &'static str;
    fn try_encode(&self, value: &Value) -> Option<Encoded>;
    // A trusted, exhaustively-fuzzed built-in codec whose fixed-point stability need not be
    // re-verified per block: its per-block losslessness is still proven by the CLMH + inverse
    // witness, so admission may skip the (expensive) idempotence re-encode. Host-supplied codecs
    // default to false and keep the per-block idempotence check, so a reckless one is still caught.
    fn trusted(&self) -> bool {
        false
    }
}

// An aggressive, only-sometimes-correct text codec: the optimizer proves decode(encode(raw)) == raw per
// block and drops any that fails, so a proposer may be reckless (wrong guesses caught, never shipped). Host
// codecs register via with_text_proposer (proven at compress time only); built-ins also self-register for portable re-verify.
pub trait TextProposer: Send + Sync {
    fn id(&self) -> &'static str;
    fn encode(&self, raw: &str) -> Option<String>;
    fn decode(&self, wire: &str) -> Option<String>;
}

type CodecClosure = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

// Proposer over host-supplied encode/decode closures, so a codec in any language (via the C ABI)
// competes in the same best-of-N search under the same per-instance proof: a buggy or hostile host
// codec is dropped, never shipped.
pub struct CallbackProposer {
    encode: CodecClosure,
    decode: CodecClosure,
}

impl CallbackProposer {
    pub fn new(
        encode: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
        decode: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            encode: Box::new(encode),
            decode: Box::new(decode),
        }
    }
}

impl TextProposer for CallbackProposer {
    fn id(&self) -> &'static str {
        "custom"
    }
    fn encode(&self, raw: &str) -> Option<String> {
        (self.encode)(raw)
    }
    fn decode(&self, wire: &str) -> Option<String> {
        (self.decode)(wire)
    }
}
