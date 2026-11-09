//! English-only wording checks. Small bundled phrase lists, case-folded plain `contains` (no regex engine), each
//! phrase explainable to the user. Wording alone is weak; `analyze` raises these only when the sender or links
//! already look wrong.

use super::{Collector, SafetyInput, Severity};

pub const MAX_TEXT: usize = 20_000;

const URGENCY: &[&str] = &[
    "within 24 hours", "within 48 hours", "within 12 hours", "immediately", "act now", "act immediately", "urgent action", "urgent attention",
    "account suspended", "account will be suspended", "account has been suspended", "account will be closed", "account has been locked",
    "account has been limited", "final notice", "final warning", "last warning", "expires today", "avoid suspension", "verify immediately",
    "failure to comply", "permanently disabled", "will be terminated",
];

const CREDENTIALS: &[&str] = &[
    "verify your password", "confirm your password", "enter your password", "update your password", "re-enter your password",
    "verify your account", "confirm your account", "confirm your identity", "verify your identity", "validate your account",
    "sign in to verify", "log in to verify", "login to verify", "log in to confirm", "login to confirm", "social security number",
    "card number", "credit card details", "cvv", "pin number", "update your payment information", "update your billing information",
    "confirm your banking", "your password has expired",
];

const PAYMENT_CHANGE: &[&str] = &[
    "new bank details", "updated bank details", "change of bank", "changed bank", "changed our bank", "bank details have changed",
    "bank account has changed", "new account number", "change our banking", "update our banking", "our banking details", "payment details have changed",
    "new payment details", "updated payment details", "new iban", "updated iban", "change in bank", "different bank account",
];

const GIFT_CARD_ASKS: &[&str] = &[
    "scratch", "send me the code", "send me a picture", "send me a photo", "photo of the card", "picture of the card", "send me the number",
    "send me the numbers", "are you available", "are you at your desk", "reply with the codes", "email me the codes",
];

const SECRECY: &[&str] = &[
    "keep this confidential", "keep this between us", "keep it confidential", "do not tell anyone", "don't tell anyone", "do not discuss this with",
    "don't discuss this with", "strictly confidential", "do not share this with anyone", "keep this private",
];

fn hits<'a>(t: &str, list: &'a [&'a str]) -> Vec<&'a str> {
    list.iter().copied().filter(|p| t.contains(p)).collect()
}

pub fn check(input: &SafetyInput, c: &mut Collector) {
    let mut t: String = input.subject.chars().chain(['\n']).chain(input.text.chars()).take(MAX_TEXT).collect::<String>().to_lowercase();
    t = t.replace(['\u{2019}', '\u{2018}'], "'");
    let urgency = hits(&t, URGENCY);
    if urgency.len() >= 2 {
        c.add("CONTENT_URGENCY", Severity::Low, format!("The message pressures you to act quickly (\"{}\", \"{}\").", urgency[0], urgency[1]));
    }
    if let Some(p) = hits(&t, CREDENTIALS).first() {
        c.add("CONTENT_CREDENTIAL_REQUEST", Severity::Low, format!("The message asks you to confirm credentials or card details (\"{p}\")."));
    }
    if let Some(p) = hits(&t, PAYMENT_CHANGE).first() {
        c.add("CONTENT_PAYMENT_CHANGE", Severity::Medium, format!("The message talks about changed bank or payment details (\"{p}\"). Confirm such changes by phone with a number you already know."));
    }
    if t.contains("gift card") || t.contains("giftcard") || t.contains("itunes card") || t.contains("google play card") {
        if let Some(p) = hits(&t, GIFT_CARD_ASKS).first() {
            c.add("CONTENT_GIFT_CARD", Severity::Medium, format!("The message mentions gift cards together with a request pattern used in scams (\"{p}\")."));
        }
    }
    if let Some(p) = hits(&t, SECRECY).first() {
        c.add("CONTENT_SECRECY", Severity::Low, format!("The message asks for secrecy (\"{p}\"), which scammers use to stop you checking with others."));
    }
    // A "Re:" subject with no In-Reply-To / References is not part of any conversation.
    let subj = input.subject.trim_start().to_lowercase();
    if subj.starts_with("re:") && !input.has_reply_headers {
        c.add("THREAD_FAKE_REPLY", Severity::Low, "The subject starts with \"Re:\" but the message does not answer any earlier message.");
    }
}
