//! Sender identity: From vs Reply-To / Return-Path, display-name tricks, spoofing of the user's own domain,
//! first-time sender. All local; the "first time" signal uses the cached mail count the caller supplies.

use super::auth::{self, Verdict};
use super::data::{is_esp_host, is_freemail, is_official, BRANDS};
use super::links::text_host;
use super::urls::{domain_of_address, reg_domain};
use super::{clean, Collector, SafetyInput, Severity};

fn related(a: &str, b: &str) -> bool {
    a.len() >= 4 && b.len() >= 4 && (a.starts_with(b) || b.starts_with(a))
}

/// Is `reg` the same organisation as `from_reg` as far as we can tell (same domain, own domain, shared name)?
fn same_org(reg: &str, from_reg: &str, own_regs: &[String]) -> bool {
    reg == from_reg || own_regs.iter().any(|o| o == reg) || related(reg.split('.').next().unwrap_or(""), from_reg.split('.').next().unwrap_or(""))
}

fn return_path_domain(rp: &str) -> Option<String> {
    let inner = rp.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!inner.is_empty() && inner.contains('@')).then(|| domain_of_address(inner))
}

pub fn check(input: &SafetyInput, c: &mut Collector) {
    let Some(from) = &input.from else { return };
    let from_dom = domain_of_address(&from.address);
    if from_dom.is_empty() {
        return;
    }
    let from_reg = reg_domain(&from_dom);
    let own_regs: Vec<String> = input.own_addresses.iter().map(|a| reg_domain(&domain_of_address(a))).filter(|d| !d.is_empty()).collect();
    let from_free = is_freemail(&from_dom);

    // Reply-To pointing elsewhere (mailing lists and helpdesk services do this legitimately).
    for rt in input.reply_to.iter().take(5) {
        let d = domain_of_address(&rt.address);
        if d.is_empty() {
            continue;
        }
        let reg = reg_domain(&d);
        if same_org(&reg, &from_reg, &own_regs) || input.is_list_mail || is_esp_host(&d) {
            continue;
        }
        let sev = if is_freemail(&d) && !from_free { Severity::High } else { Severity::Medium };
        c.add("SENDER_REPLYTO_MISMATCH", sev, format!("Replies would go to {} but the message comes from {}.", clean(&d, 50), clean(&from_dom, 50)));
    }

    // Return-Path (envelope sender): mailing services use their own, so this only weakly matters.
    if let Some(d) = return_path_domain(&input.return_path) {
        let reg = reg_domain(&d);
        if !same_org(&reg, &from_reg, &own_regs) && !input.is_list_mail && !is_esp_host(&d) {
            c.add("SENDER_RETURNPATH_MISMATCH", Severity::Low, format!("The bounce address is at {} while the sender is {}.", clean(&d, 50), clean(&from_dom, 50)));
        }
    }

    // Display name.
    if let Some(name) = from.name.as_deref().map(|n| clean(n, 120)).filter(|n| !n.is_empty()) {
        let lname = name.to_lowercase();
        let mut address_claim = false;
        for tok in lname.split_whitespace() {
            let tok = tok.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '(' | ')' | '<' | '>' | ',' | ';' | ':'));
            if let Some((_, dom)) = tok.rsplit_once('@') {
                let dom = dom.trim_end_matches('.');
                if dom.contains('.') && reg_domain(dom) != from_reg {
                    c.add("SENDER_DISPLAY_NAME_ADDRESS", Severity::High, format!("The sender's name shows the address \"{}\", but the message really comes from {}.", clean(tok, 60), clean(&from.address, 60)));
                    address_claim = true;
                    break;
                }
            }
        }
        if !address_claim {
            if let Some(h) = text_host(&lname) {
                let reg = reg_domain(&h);
                if !same_org(&reg, &from_reg, &own_regs) && !is_esp_host(&from_dom) && !input.is_list_mail {
                    c.add("SENDER_DISPLAY_NAME_ADDRESS", Severity::Medium, format!("The sender's name mentions {}, but the message comes from {}.", clean(&h, 50), clean(&from_dom, 50)));
                    address_claim = true;
                }
            }
        }
        // A well-known brand in the name, but the address belongs to someone else ("X via Y" mail is normal).
        if !address_claim && !lname.contains(" via ") {
            let words: Vec<&str> = lname.split(|ch: char| !ch.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
            if let Some((b, _)) = BRANDS.iter().find(|(b, _)| b.len() >= 5 && words.contains(b)) {
                let host_has_brand = from_dom.contains(*b) || is_official(b, &from_reg);
                if !host_has_brand && !is_esp_host(&from_dom) {
                    let sev = if from_free { Severity::High } else if input.is_list_mail { Severity::Low } else { Severity::Medium };
                    c.add("SENDER_DISPLAY_NAME_BRAND", sev, format!("The sender's name says \"{}\" but the address is at {}, which is not a {} domain.", clean(&name, 40), clean(&from_dom, 50), b));
                }
            }
        }
    }

    // From = the user's own domain, yet the user's own server saw no passing authentication.
    if own_regs.contains(&from_reg) {
        let ar = auth::parse(&input.authentication_results, &input.trusted_auth_hosts);
        if ar.trusted && ![ar.spf, ar.dkim, ar.dmarc].contains(&Verdict::Pass) {
            c.add("SENDER_SPOOFS_OWN_DOMAIN", Severity::High, format!("The message claims to come from your own domain ({}), but your mail server recorded no passing SPF, DKIM or DMARC for it.", clean(&from_dom, 50)));
        }
    }

    if input.cached_from_count.is_some_and(|n| n <= 1) {
        c.add("SENDER_FIRST_TIME", Severity::Low, format!("No earlier mail from {} is stored on this Mac.", clean(&from.address, 60)));
    }
}
