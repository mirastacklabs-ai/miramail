use super::*;

fn addr(name: Option<&str>, a: &str) -> EmailAddress {
    EmailAddress { name: name.map(str::to_string), address: a.to_string() }
}

/// A calm baseline: known sender, passing authentication from the account's own server, no links.
fn base() -> SafetyInput {
    SafetyInput {
        from: Some(addr(Some("Alice Example"), "alice@example.com")),
        authentication_results: "mx.mailhost.test; spf=pass smtp.mailfrom=example.com; dkim=pass header.d=example.com; dmarc=pass header.from=example.com".into(),
        trusted_auth_hosts: vec!["mx.mailhost.test".into(), "mailhost.test".into()],
        own_addresses: vec!["me@mailhost.test".into()],
        subject: "Lunch on Friday?".into(),
        text: "Hi, are we still on for lunch on Friday? Alice".into(),
        cached_from_count: Some(12),
        ..Default::default()
    }
}

fn link(href: &str, text: &str) -> LinkFact {
    LinkFact { href: href.into(), text: text.into() }
}

fn att(name: &str, mime: &str) -> AttachFact {
    AttachFact { name: name.into(), mime: mime.into() }
}

fn codes(r: &SafetyReport) -> Vec<&str> {
    r.reasons.iter().map(|x| x.code.as_str()).collect()
}

fn has(r: &SafetyReport, code: &str) -> bool {
    r.reasons.iter().any(|x| x.code == code)
}

fn sev(r: &SafetyReport, code: &str) -> Severity {
    r.reasons.iter().find(|x| x.code == code).unwrap_or_else(|| panic!("{code} missing in {:?}", codes(r))).severity
}

#[test]
fn calm_baseline_finds_nothing() {
    let r = analyze(&base());
    assert_eq!(r.level, Level::NoneFound, "{:?}", codes(&r));
    assert_eq!(r.score, 0);
}

#[test]
fn there_is_no_safe_level() {
    // The type has exactly three levels and the lowest is "none found"; serialised names are stable.
    assert_eq!(serde_json::to_string(&Level::NoneFound).unwrap(), "\"none_found\"");
    assert_eq!(serde_json::to_string(&Level::Caution).unwrap(), "\"caution\"");
    assert_eq!(serde_json::to_string(&Level::Warning).unwrap(), "\"warning\"");
}

// ---------------------------------------------------------------- authentication

#[test]
fn dmarc_fail_from_own_server_is_flagged() {
    let mut i = base();
    i.authentication_results = "mx.mailhost.test; spf=fail smtp.mailfrom=x@evil.test; dkim=none; dmarc=fail header.from=example.com".into();
    let r = analyze(&i);
    assert!(has(&r, "AUTH_DMARC_FAIL"));
    assert!(has(&r, "AUTH_SPF_FAIL"));
    assert_eq!(sev(&r, "AUTH_DMARC_FAIL"), Severity::High);
    assert_ne!(r.level, Level::NoneFound);
}

#[test]
fn untrusted_authserv_id_is_unknown_never_pass_or_fail() {
    let mut i = base();
    i.authentication_results = "evil.test; dmarc=pass; dkim=pass; spf=pass".into();
    let r = analyze(&i);
    assert!(has(&r, "AUTH_UNKNOWN"));
    assert_eq!(sev(&r, "AUTH_UNKNOWN"), Severity::Info);
    assert!(!has(&r, "AUTH_DMARC_FAIL"));
    assert_eq!(r.level, Level::NoneFound);
    i.authentication_results = String::new();
    assert!(has(&analyze(&i), "AUTH_UNKNOWN"));
}

#[test]
fn spf_fail_after_forwarding_with_dmarc_pass_is_weak() {
    let mut i = base();
    i.authentication_results = "mx.mailhost.test; spf=fail smtp.mailfrom=fwd.test; dkim=pass header.d=example.com; dmarc=pass".into();
    let r = analyze(&i);
    assert_eq!(sev(&r, "AUTH_SPF_FAIL"), Severity::Low);
    assert_eq!(r.level, Level::NoneFound);
}

#[test]
fn no_auth_at_all_from_trusted_server_is_a_low_note() {
    let mut i = base();
    i.authentication_results = "mx.mailhost.test; none".into();
    let r = analyze(&i);
    assert_eq!(sev(&r, "AUTH_NONE"), Severity::Low);
    assert_eq!(r.level, Level::NoneFound);
}

// ---------------------------------------------------------------- sender identity

#[test]
fn display_name_with_foreign_address() {
    let mut i = base();
    i.from = Some(addr(Some("ceo@corp-example.com"), "xk3@gmail.com"));
    let r = analyze(&i);
    assert_eq!(sev(&r, "SENDER_DISPLAY_NAME_ADDRESS"), Severity::High);
}

#[test]
fn display_name_with_same_domain_address_is_fine() {
    let mut i = base();
    i.from = Some(addr(Some("alice@example.com"), "alice@example.com"));
    assert!(!has(&analyze(&i), "SENDER_DISPLAY_NAME_ADDRESS"));
}

#[test]
fn display_name_brand_on_freemail_and_on_unrelated_domain() {
    let mut i = base();
    i.from = Some(addr(Some("PayPal Service"), "support.team@gmail.com"));
    let r = analyze(&i);
    assert_eq!(sev(&r, "SENDER_DISPLAY_NAME_BRAND"), Severity::High);
    i.from = Some(addr(Some("Apple Support"), "help@random-shop.biz"));
    assert_eq!(sev(&analyze(&i), "SENDER_DISPLAY_NAME_BRAND"), Severity::Medium);
}

#[test]
fn real_brand_senders_and_via_names_are_not_flagged() {
    for (n, a) in [
        ("PayPal", "service@intl.paypal.com"),
        ("Apple", "no_reply@email.apple.com"),
        ("Google", "no-reply@accounts.google.com"),
        ("Facebook", "notification@facebookmail.com"),
        ("Microsoft account team", "account-security-noreply@accountprotection.microsoft.com"),
        ("Dropbox via Jane Doe", "no-reply@jane-corp.example"),
        ("Amazon Web Services", "no-reply@sns.amazonaws.com"),
    ] {
        let mut i = base();
        i.from = Some(addr(Some(n), a));
        let r = analyze(&i);
        assert!(!has(&r, "SENDER_DISPLAY_NAME_BRAND"), "{n} <{a}>: {:?}", codes(&r));
        assert!(!has(&r, "DOMAIN_BRAND_IN_SUBDOMAIN"), "{n} <{a}>: {:?}", codes(&r));
    }
}

#[test]
fn reply_to_other_domain_is_flagged_but_freemail_reply_to_for_company_is_worse() {
    let mut i = base();
    i.reply_to = vec![addr(None, "bob@other-company.net")];
    let r = analyze(&i);
    assert_eq!(sev(&r, "SENDER_REPLYTO_MISMATCH"), Severity::Medium);
    i.reply_to = vec![addr(None, "ceo.office@gmail.com")];
    assert_eq!(sev(&analyze(&i), "SENDER_REPLYTO_MISMATCH"), Severity::High);
}

#[test]
fn reply_to_guards_against_false_positives() {
    // Mailing-list Reply-To to the list address.
    let mut i = base();
    i.is_list_mail = true;
    i.reply_to = vec![addr(None, "dev-list@lists.project.org")];
    assert!(!has(&analyze(&i), "SENDER_REPLYTO_MISMATCH"));
    // Own-domain alias.
    let mut i = base();
    i.from = Some(addr(Some("Me"), "me@mailhost.test"));
    i.reply_to = vec![addr(None, "me+alias@mailhost.test")];
    assert!(!has(&analyze(&i), "SENDER_REPLYTO_MISMATCH"));
    // Reply-To to another of the user's own domains (saved identity).
    let mut i = base();
    i.own_addresses.push("me@my-other-domain.org".into());
    i.reply_to = vec![addr(None, "me@my-other-domain.org")];
    assert!(!has(&analyze(&i), "SENDER_REPLYTO_MISMATCH"));
    // Helpdesk service.
    let mut i = base();
    i.reply_to = vec![addr(None, "support+id123@example.zendesk.com")];
    assert!(!has(&analyze(&i), "SENDER_REPLYTO_MISMATCH"));
    // Same organisation, different brand domain (company.com vs company-mail.com).
    let mut i = base();
    i.reply_to = vec![addr(None, "team@example-support.com")];
    assert!(!has(&analyze(&i), "SENDER_REPLYTO_MISMATCH"));
}

#[test]
fn return_path_mismatch_is_low_and_skips_esp_and_lists() {
    let mut i = base();
    i.return_path = "<bounce@unrelated-mailer.biz>".into();
    let r = analyze(&i);
    assert_eq!(sev(&r, "SENDER_RETURNPATH_MISMATCH"), Severity::Low);
    assert_eq!(r.level, Level::NoneFound);
    i.return_path = "<bounce+abc@bounce.sendgrid.net>".into();
    assert!(!has(&analyze(&i), "SENDER_RETURNPATH_MISMATCH"));
    i.return_path = "<>".into();
    assert!(!has(&analyze(&i), "SENDER_RETURNPATH_MISMATCH"));
    i.return_path = "<bounce@bounces.example.com>".into();
    assert!(!has(&analyze(&i), "SENDER_RETURNPATH_MISMATCH"));
}

#[test]
fn spoofing_the_users_own_domain() {
    let mut i = base();
    i.from = Some(addr(Some("Boss"), "boss@mailhost.test"));
    i.authentication_results = "mx.mailhost.test; spf=none; dkim=none; dmarc=none".into();
    assert!(has(&analyze(&i), "SENDER_SPOOFS_OWN_DOMAIN"));
    i.authentication_results = "mx.mailhost.test; dkim=pass header.d=mailhost.test".into();
    assert!(!has(&analyze(&i), "SENDER_SPOOFS_OWN_DOMAIN"));
    // No trusted header at all: not judged.
    i.authentication_results = String::new();
    assert!(!has(&analyze(&i), "SENDER_SPOOFS_OWN_DOMAIN"));
}

#[test]
fn first_time_sender_is_only_a_low_note() {
    let mut i = base();
    i.cached_from_count = Some(1);
    let r = analyze(&i);
    assert_eq!(sev(&r, "SENDER_FIRST_TIME"), Severity::Low);
    assert_eq!(r.level, Level::NoneFound);
    i.cached_from_count = None;
    assert!(!has(&analyze(&i), "SENDER_FIRST_TIME"));
}

// ---------------------------------------------------------------- domains and links

#[test]
fn homograph_cyrillic_a_in_link_host() {
    let mut i = base();
    i.links = vec![link("https://xn--pypal-4ve.com/login", "paypal.com")];
    let r = analyze(&i);
    assert!(has(&r, "DOMAIN_MIXED_SCRIPT") || has(&r, "DOMAIN_CONFUSABLE") || has(&r, "DOMAIN_PUNYCODE"), "{:?}", codes(&r));
    // Raw unicode form: Latin letters with a Cyrillic "а" in the middle.
    let mut i = base();
    i.links = vec![link("https://p\u{430}ypal.com/signin", "Sign in")];
    let r = analyze(&i);
    assert!(has(&r, "DOMAIN_MIXED_SCRIPT"), "{:?}", codes(&r));
    assert_eq!(sev(&r, "DOMAIN_MIXED_SCRIPT"), Severity::High);
}

#[test]
fn whole_script_cyrillic_apple_lookalike() {
    let mut i = base();
    i.links = vec![link("https://xn--80ak6aa92e.com/", "Apple ID")];
    let r = analyze(&i);
    assert!(has(&r, "DOMAIN_CONFUSABLE"), "{:?}", codes(&r));
}

#[test]
fn ascii_lookalikes_of_brands_and_of_own_domain() {
    for h in ["https://paypa1.com/x", "https://rnicrosoft.com/", "https://g00gle.com/", "https://micros0ft-online.com/"] {
        let mut i = base();
        i.links = vec![link(h, "click here")];
        let r = analyze(&i);
        assert!(has(&r, "DOMAIN_CONFUSABLE") || has(&r, "DOMAIN_BRAND_IN_SUBDOMAIN") || has(&r, "DOMAIN_EDIT_DISTANCE"), "{h}: {:?}", codes(&r));
    }
    // One letter off a brand.
    let mut i = base();
    i.links = vec![link("https://paypall.com/", "x")];
    assert!(has(&analyze(&i), "DOMAIN_EDIT_DISTANCE"));
    // Sender imitating the user's own domain.
    let mut i = base();
    i.from = Some(addr(Some("IT"), "it@rnailhost.test"));
    assert!(has(&analyze(&i), "DOMAIN_CONFUSABLE"));
}

#[test]
fn real_brand_domains_and_services_are_not_lookalikes() {
    for h in [
        "https://www.paypal.com/signin", "https://paypal.co.uk/", "https://www.paypay.ne.jp/", "https://accounts.google.com/", "https://login.microsoftonline.com/",
        "https://aws.amazon.com/", "https://s3.amazonaws.com/bucket/file", "https://www.1password.com/", "https://github.com/x", "https://githubusercontent.com/a",
        "https://appleid.apple.com/", "https://www.linkedin.com/in/x",
    ] {
        let mut i = base();
        i.links = vec![link(h, "link")];
        let r = analyze(&i);
        assert_eq!(r.level, Level::NoneFound, "{h}: {:?}", codes(&r));
    }
}

#[test]
fn brand_in_subdomain_chain() {
    let mut i = base();
    i.links = vec![link("https://paypal.com.secure-login.xyz/", "Log in")];
    let r = analyze(&i);
    assert!(has(&r, "DOMAIN_BRAND_IN_SUBDOMAIN"), "{:?}", codes(&r));
    assert!(has(&r, "DOMAIN_SUSPICIOUS_TLD"));
}

#[test]
fn text_href_mismatch_with_brand_is_high() {
    let mut i = base();
    i.links = vec![link("https://evil-site.test/login", "https://www.paypal.com/myaccount")];
    let r = analyze(&i);
    assert_eq!(sev(&r, "LINK_TEXT_MISMATCH"), Severity::High);
    assert!(r.reasons.iter().any(|x| x.detail.contains("paypal.com")));
    i.links = vec![link("https://evil-site.test/login", "Visit www.example.com now")];
    assert_eq!(sev(&analyze(&i), "LINK_TEXT_MISMATCH"), Severity::High); // text shows the sender's own domain
}

#[test]
fn matching_text_and_href_is_fine() {
    let mut i = base();
    i.links = vec![
        link("https://www.example.com/news", "www.example.com"),
        link("https://shop.example.com/", "https://example.com/shop"),
        link("https://example.com/a", "Read the example.com blog"),
        link("https://example.com/a2", "Read more"),
        link("mailto:a@b.test", "a@b.test"),
        link("https://other.test/", "version 2.0 notes"),
    ];
    let r = analyze(&i);
    assert!(!has(&r, "LINK_TEXT_MISMATCH"), "{:?}", codes(&r));
}

#[test]
fn official_alternate_domain_for_a_brand_text_is_fine() {
    let mut i = base();
    i.links = vec![link("https://amzn.to/3abc", "amazon.com")];
    let r = analyze(&i);
    assert!(!has(&r, "LINK_TEXT_MISMATCH"), "{:?}", codes(&r));
}

#[test]
fn esp_tracking_redirects_never_warn() {
    let mut i = base();
    i.is_list_mail = true;
    i.links = vec![
        link("https://example.us12.list-manage.com/track/click?u=abc&id=1", "https://www.example.com/sale"),
        link("https://u1234.ct.sendgrid.net/ls/click?upn=abcdef", "example.com"),
        link("https://emea01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2F&data=x", "https://example.com/"),
        link("https://links.klclick.com/x?url=https%3A%2F%2Fother.test%2F", "Shop now at example.com"),
        link("https://urldefense.com/v3/__https://example.com/x__;!!abc", "example.com/x"),
    ];
    let r = analyze(&i);
    assert!(!has(&r, "LINK_TEXT_MISMATCH") || sev(&r, "LINK_TEXT_MISMATCH") <= Severity::Medium, "{:?}", codes(&r));
    assert_ne!(r.level, Level::Warning, "{:?}", codes(&r));
    assert!(!has(&r, "LINK_OPEN_REDIRECT"));
    assert!(has(&r, "LINK_TRACKING_REDIRECT"));
    assert_eq!(sev(&r, "LINK_TRACKING_REDIRECT"), Severity::Info);
}

#[test]
fn unknown_click_domain_showing_the_senders_domain_is_caution_not_warning() {
    let mut i = base();
    i.links = vec![link("https://click.e-mailer-unknown.net/x", "www.example.com")];
    let r = analyze(&i);
    assert_eq!(r.level, Level::Caution, "{:?}", codes(&r));
}

#[test]
fn ip_literals_all_forms() {
    for h in [
        "http://127.0.0.1/a", "http://2130706433/", "http://0x7f.1/", "http://0177.0.0.1/", "http://127.1/", "http://[::ffff:127.0.0.1]/", "http://[::ffff:7f00:1]/",
        "http://[64:ff9b::a9fe:a9fe]/", "http://[2001:db8::5]/", "http://203.0.113.9:8080/pay",
    ] {
        let mut i = base();
        i.links = vec![link(h, "Open document")];
        let r = analyze(&i);
        assert_eq!(sev(&r, "LINK_IP_LITERAL"), Severity::High, "{h}");
    }
    let mut i = base();
    i.links = vec![link("http://0x7f.1/", "x")];
    assert!(has(&analyze(&i), "LINK_OBFUSCATED_HOST"));
    let mut i = base();
    i.links = vec![link("http://192.168.1.1/", "x")];
    assert!(!has(&analyze(&i), "LINK_OBFUSCATED_HOST"));
}

#[test]
fn ipv6_mapped_detail_names_the_embedded_address() {
    let mut i = base();
    i.links = vec![link("http://[::ffff:7f00:1]/", "x")];
    let r = analyze(&i);
    assert!(r.reasons.iter().any(|x| x.code == "LINK_IP_LITERAL" && x.detail.contains("127.0.0.1")), "{:?}", r.reasons);
}

#[test]
fn userinfo_trick_has_a_hard_floor_into_warning() {
    let mut i = base();
    i.links = vec![link("https://paypal.com@evil-host.test/login", "https://paypal.com")];
    let r = analyze(&i);
    assert!(has(&r, "LINK_USERINFO"));
    assert_eq!(r.level, Level::Warning);
    assert!(r.score >= 60);
}

#[test]
fn trailing_dot_port_percent_host_and_long_urls() {
    let mut i = base();
    i.links = vec![link("https://evil.test./x", "a"), link("https://evil2.test:8443/x", "b"), link("http://ev%69l3.test/", "c"), link(&format!("https://evil4.test/{}", "a".repeat(2100)), "d")];
    let r = analyze(&i);
    for code in ["LINK_TRAILING_DOT", "LINK_NONSTANDARD_PORT", "LINK_OBFUSCATED_HOST", "LINK_VERY_LONG"] {
        assert!(has(&r, code), "{code}: {:?}", codes(&r));
    }
    // Default ports are fine.
    let mut i = base();
    i.links = vec![link("https://fine.test:443/x", "a"), link("http://fine2.test:80/x", "b")];
    assert!(!has(&analyze(&i), "LINK_NONSTANDARD_PORT"));
}

#[test]
fn shorteners_and_open_redirects() {
    let mut i = base();
    i.links = vec![link("https://bit.ly/3xYz", "Click")];
    let r = analyze(&i);
    assert_eq!(sev(&r, "LINK_SHORTENER"), Severity::Low);
    assert_eq!(r.level, Level::NoneFound);
    let mut i = base();
    i.links = vec![link("https://some-site.test/out?url=https%3A%2F%2Fevil-landing.example%2Fx", "Click")];
    assert_eq!(sev(&analyze(&i), "LINK_OPEN_REDIRECT"), Severity::Medium);
    // Redirect to the same site is normal.
    let mut i = base();
    i.links = vec![link("https://some-site.test/out?url=https%3A%2F%2Fwww.some-site.test%2Fpage", "Click")];
    assert!(!has(&analyze(&i), "LINK_OPEN_REDIRECT"));
}

#[test]
fn dangerous_schemes_counted_from_raw_html() {
    let mut i = base();
    i.dangerous_scheme_links = 2;
    let r = analyze(&i);
    assert_eq!(sev(&r, "LINK_DANGEROUS_SCHEME"), Severity::High);
    let raw = r#"<a href="javascript:alert(1)">x</a><a href="https://ok.test">y</a>"#;
    assert_eq!(html::dangerous_scheme_count(raw), 1);
}

#[test]
fn non_http_links_are_ignored() {
    let mut i = base();
    i.links = vec![link("mailto:a@b.test", "a@b.test"), link("tel:+15551234", "call"), link("#top", "top"), link("cid:logo", "logo"), link("", "")];
    assert_eq!(analyze(&i).level, Level::NoneFound);
}

// ---------------------------------------------------------------- attachments

#[test]
fn double_extension_and_executables_force_warning() {
    for n in ["invoice.pdf.exe", "scan.jpg.scr", "report.docx.js", "photo.png.app", "statement.pdf.vbs"] {
        let mut i = base();
        i.attachments = vec![att(n, "application/octet-stream")];
        let r = analyze(&i);
        assert!(has(&r, "ATT_DOUBLE_EXT"), "{n}");
        assert!(has(&r, "ATT_EXECUTABLE"), "{n}");
        assert_eq!(r.level, Level::Warning, "{n}");
        assert!(r.score >= 80);
    }
}

#[test]
fn risky_attachment_types() {
    for (n, code) in [
        ("setup.exe", "ATT_EXECUTABLE"), ("run.command", "ATT_EXECUTABLE"), ("x.jar", "ATT_EXECUTABLE"), ("a.lnk", "ATT_EXECUTABLE"), ("m.vbs", "ATT_EXECUTABLE"),
        ("p.ps1", "ATT_EXECUTABLE"), ("update.pkg", "ATT_EXECUTABLE"), ("disk.iso", "ATT_DISK_IMAGE"), ("tool.dmg", "ATT_DISK_IMAGE"), ("boot.img", "ATT_DISK_IMAGE"),
        ("login.html", "ATT_HTML"), ("form.HTM", "ATT_HTML"), ("budget.xlsm", "ATT_MACRO"), ("letter.docm", "ATT_MACRO"), ("pack.zip", "ATT_ARCHIVE"), ("pack.7z", "ATT_ARCHIVE"),
    ] {
        let mut i = base();
        i.attachments = vec![att(n, "application/octet-stream")];
        assert!(has(&analyze(&i), code), "{n} should give {code}");
    }
}

#[test]
fn bidi_override_in_attachment_name() {
    let mut i = base();
    i.attachments = vec![att("annex\u{202e}fdp.exe", "application/octet-stream")];
    let r = analyze(&i);
    assert!(has(&r, "ATT_BIDI_NAME"));
    assert_eq!(r.level, Level::Warning);
    assert!(!r.reasons.iter().any(|x| x.detail.contains('\u{202e}')), "bidi char must be stripped from the text");
}

#[test]
fn archive_with_password_in_body() {
    let mut i = base();
    i.attachments = vec![att("docs.zip", "application/zip")];
    i.text = "Please open the attachment. The password is 1234.".into();
    assert!(has(&analyze(&i), "ATT_ENCRYPTED_ARCHIVE_PASSWORD"));
    i.text = "Photos attached.".into();
    assert!(!has(&analyze(&i), "ATT_ENCRYPTED_ARCHIVE_PASSWORD"));
}

#[test]
fn mime_mismatch_and_benign_attachments() {
    let mut i = base();
    i.attachments = vec![att("receipt.pdf", "application/x-msdownload")];
    assert!(has(&analyze(&i), "ATT_MIME_MISMATCH"));
    let mut i = base();
    i.attachments = vec![
        att("invoice-1042.pdf", "application/pdf"), att("photo.JPG", "image/jpeg"), att("notes.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        att("table.xlsx", "application/octet-stream"), att("invite.ics", "text/calendar"), att("README", "text/plain"), att("v1.2.final.pdf", "application/pdf"),
    ];
    let r = analyze(&i);
    assert_eq!(r.level, Level::NoneFound, "{:?}", codes(&r));
    assert!(r.reasons.is_empty(), "{:?}", codes(&r));
}

// ---------------------------------------------------------------- content

#[test]
fn urgency_needs_two_phrases_and_stays_low_alone() {
    let mut i = base();
    i.text = "Your account has been locked. Act now or it will be closed.".into();
    let r = analyze(&i);
    assert_eq!(sev(&r, "CONTENT_URGENCY"), Severity::Low);
    assert_eq!(r.level, Level::NoneFound);
    i.text = "Please reply immediately.".into();
    assert!(!has(&analyze(&i), "CONTENT_URGENCY"));
}

#[test]
fn credential_request_is_raised_by_a_bad_sender_but_alone_is_low() {
    let mut i = base();
    i.text = "Please verify your password to keep your mailbox.".into();
    let r = analyze(&i);
    assert_eq!(sev(&r, "CONTENT_CREDENTIAL_REQUEST"), Severity::Low);
    assert_eq!(r.level, Level::NoneFound);
    i.reply_to = vec![addr(None, "ceo.office@gmail.com")];
    let r = analyze(&i);
    assert_eq!(sev(&r, "CONTENT_CREDENTIAL_REQUEST"), Severity::Medium);
    assert_eq!(r.level, Level::Warning, "{:?}", codes(&r));
}

#[test]
fn bec_payment_change_with_reply_to_diversion_is_a_warning() {
    let mut i = base();
    i.text = "Hello, our bank details have changed. Please use the new account for the next transfer and keep this confidential.".into();
    i.reply_to = vec![addr(None, "payments.dept@gmail.com")];
    i.cached_from_count = Some(1);
    let r = analyze(&i);
    assert!(has(&r, "CONTENT_PAYMENT_CHANGE"));
    assert!(has(&r, "CONTENT_SECRECY"));
    assert_eq!(r.level, Level::Warning, "{:?}", codes(&r));
}

#[test]
fn payment_change_wording_alone_is_a_caution_at_most() {
    let mut i = base();
    i.text = "Quick note: our bank details have changed, see the invoice.".into();
    let r = analyze(&i);
    assert_eq!(sev(&r, "CONTENT_PAYMENT_CHANGE"), Severity::Medium);
    assert_eq!(r.level, Level::Caution);
}

#[test]
fn gift_card_scam_pattern_but_not_a_promo() {
    let mut i = base();
    i.text = "Are you available? I need you to buy 5 gift cards and send me the codes.".into();
    assert!(has(&analyze(&i), "CONTENT_GIFT_CARD"));
    i.text = "Treat someone: our gift cards are 20% off this week.".into();
    assert!(!has(&analyze(&i), "CONTENT_GIFT_CARD"));
}

#[test]
fn fake_reply_subject() {
    let mut i = base();
    i.subject = "Re: Invoice".into();
    i.has_reply_headers = false;
    assert!(has(&analyze(&i), "THREAD_FAKE_REPLY"));
    i.has_reply_headers = true;
    assert!(!has(&analyze(&i), "THREAD_FAKE_REPLY"));
}

// ---------------------------------------------------------------- benign mail must not warn

#[test]
fn benign_newsletter() {
    let mut i = base();
    i.from = Some(addr(Some("Acme Weekly"), "news@mail.acme-example.com"));
    i.is_list_mail = true;
    i.return_path = "<bounce-123@bounces.acme-example.com>".into();
    i.reply_to = vec![addr(None, "hello@acme-example.com")];
    i.authentication_results = "mx.mailhost.test; spf=pass smtp.mailfrom=bounces.acme-example.com; dkim=pass header.d=acme-example.com; dmarc=pass".into();
    i.subject = "This week at Acme: 20% off, final hours".into();
    i.text = "Final hours for our sale! Shop now. Unsubscribe any time. Act now to save.".into();
    i.links = vec![
        link("https://acme.us3.list-manage.com/track/click?u=1&id=2", "Shop the sale"),
        link("https://acme.us3.list-manage.com/unsubscribe?u=1", "Unsubscribe"),
        link("https://www.acme-example.com/sale", "www.acme-example.com/sale"),
        link("https://twitter.com/acme", "Twitter"),
        link("https://bit.ly/acme-deal", "Deal"),
    ];
    i.attachments = vec![];
    i.cached_from_count = Some(3);
    let r = analyze(&i);
    assert_eq!(r.level, Level::NoneFound, "{:?} score {}", codes(&r), r.score);
}

#[test]
fn benign_newsletter_first_message_still_not_a_warning() {
    let mut i = base();
    i.from = Some(addr(Some("Acme Weekly"), "news@mail.acme-example.com"));
    i.is_list_mail = true;
    i.cached_from_count = Some(1);
    i.text = "Welcome! Confirm your subscription to start. Final notice: act now.".into();
    i.links = vec![link("https://acme.us3.list-manage.com/confirm?u=1", "Confirm your subscription"), link("https://t.co/abc", "https://www.acme-example.com")];
    let r = analyze(&i);
    assert_ne!(r.level, Level::Warning, "{:?}", codes(&r));
}

#[test]
fn benign_password_reset() {
    let mut i = base();
    i.from = Some(addr(Some("Example Service"), "no-reply@accounts.service-example.com"));
    i.authentication_results = "mx.mailhost.test; spf=pass; dkim=pass header.d=service-example.com; dmarc=pass".into();
    i.subject = "Reset your password".into();
    i.text = "We received a request to reset your password. Click the button below to choose a new password. This link expires in 24 hours. If you did not request it, ignore this email. Do not share this link.".into();
    i.links = vec![link("https://accounts.service-example.com/reset?token=Zm9vYmFyMTIzNDU2Nzg5MA", "Reset password"), link("https://service-example.com/help", "service-example.com/help")];
    i.cached_from_count = Some(1);
    let r = analyze(&i);
    assert_eq!(r.level, Level::NoneFound, "{:?} score {}", codes(&r), r.score);
}

#[test]
fn benign_two_factor_code_and_receipt_with_pdf() {
    let mut i = base();
    i.from = Some(addr(Some("Shop"), "orders@shop-example.com"));
    i.authentication_results = "mx.mailhost.test; spf=pass; dkim=pass header.d=shop-example.com; dmarc=pass".into();
    i.subject = "Your receipt".into();
    i.text = "Thanks for your order. Your verification code is 482913. Receipt attached.".into();
    i.attachments = vec![att("receipt-8841.pdf", "application/pdf")];
    i.links = vec![link("https://shop-example.com/orders/8841", "View order")];
    i.cached_from_count = Some(5);
    assert_eq!(analyze(&i).level, Level::NoneFound);
}

#[test]
fn mailing_list_post_through_a_list_server() {
    let mut i = base();
    i.from = Some(addr(Some("Dana via dev-list"), "dev-list@lists.project.org"));
    i.is_list_mail = true;
    i.reply_to = vec![addr(None, "dev-list@lists.project.org")];
    i.authentication_results = "mx.mailhost.test; spf=pass; dkim=fail header.d=lists.project.org; dmarc=none".into();
    i.return_path = "<dev-list-bounces@lists.project.org>".into();
    let r = analyze(&i);
    assert_eq!(r.level, Level::NoneFound, "{:?}", codes(&r));
}

// ---------------------------------------------------------------- scoring

#[test]
fn noisy_or_is_bounded_and_floors_apply() {
    let low = |n| (0..n).map(|k| Reason { code: format!("X{k}"), severity: Severity::Low, title: String::new(), detail: String::new(), count: 1, floor: 0 }).collect::<Vec<_>>();
    assert_eq!(score_of(&low(0)), 0);
    assert!(score_of(&low(3)) < 25);
    assert!(score_of(&low(100)) <= 100);
    let one_high = vec![Reason { code: "H".into(), severity: Severity::High, title: String::new(), detail: String::new(), count: 1, floor: 0 }];
    assert_eq!(score_of(&one_high), 40);
    assert_eq!(level_of(19), Level::NoneFound);
    assert_eq!(level_of(20), Level::Caution);
    assert_eq!(level_of(49), Level::Caution);
    assert_eq!(level_of(50), Level::Warning);
}

#[test]
fn repeated_codes_merge_and_count() {
    let mut i = base();
    i.links = vec![link("https://bit.ly/a", "a"), link("https://bit.ly/b", "b"), link("https://tinyurl.com/c", "c")];
    let r = analyze(&i);
    let s = r.reasons.iter().find(|x| x.code == "LINK_SHORTENER").unwrap();
    assert_eq!(s.count, 3);
    assert_eq!(r.reasons.iter().filter(|x| x.code == "LINK_SHORTENER").count(), 1);
}

#[test]
fn reasons_are_sorted_most_severe_first_and_details_are_plain_text() {
    let mut i = base();
    i.from = Some(addr(Some("<b>PayPal</b>\u{202e}"), "x@gmail.com"));
    i.links = vec![link("https://bit.ly/a", "a"), link("http://10.0.0.1/", "b")];
    i.attachments = vec![att("a<script>.pdf.exe", "application/octet-stream")];
    let r = analyze(&i);
    let sevs: Vec<Severity> = r.reasons.iter().map(|x| x.severity).collect();
    assert!(sevs.windows(2).all(|w| w[0] >= w[1]), "{sevs:?}");
    for x in &r.reasons {
        assert!(!x.detail.chars().any(|c| c.is_control() || is_bidi(c)), "{}", x.detail);
        assert!(x.detail.chars().count() < 400, "{}", x.detail);
    }
}

#[test]
fn clean_strips_clips_and_collapses() {
    assert_eq!(clean("a\u{202e}b\n\tc   d", 50), "a b c d");
    assert_eq!(clean(&"x".repeat(100), 10).chars().count(), 10);
    assert!(clean(&"x".repeat(100), 10).ends_with('…'));
}

#[test]
fn degenerate_input_does_not_panic() {
    let r = analyze(&SafetyInput::default());
    assert_eq!(r.level, Level::NoneFound);
    let mut i = base();
    i.from = Some(addr(None, "no-at-sign"));
    i.links = vec![link("https://", ""), link("::::", "x"), link("http://[::1", "y"), link("http://a b/", "z")];
    i.attachments = vec![att("", ""), att(".", "x"), att("....", "y")];
    let _ = analyze(&i);
}

// ---------------------------------------------------------------- end to end through the parser

#[test]
fn build_input_from_a_raw_message() {
    let raw = b"From: \"PayPal Security\" <alerts@paypa1-secure.example>\r\nReply-To: ceo.office@gmail.com\r\nTo: me@mailhost.test\r\nSubject: Re: Action required\r\nReturn-Path: <bounce@other-mailer.test>\r\nAuthentication-Results: mx.mailhost.test; spf=fail smtp.mailfrom=other-mailer.test; dkim=none; dmarc=fail header.from=paypa1-secure.example\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Your account has been locked. Act now: <a href=\"http://192.0.2.7/login\">https://www.paypal.com/signin</a> <a href=\"javascript:alert(1)\">x</a></p>\r\n--b\r\nContent-Type: application/octet-stream; name=\"invoice.pdf.exe\"\r\nContent-Disposition: attachment; filename=\"invoice.pdf.exe\"\r\n\r\nMZ\r\n--b--\r\n";
    let parsed = crate::mime::parse_message(raw).unwrap();
    let extra = crate::mime::parse_extra_headers(raw);
    let input = build_input(&parsed, &extra, &["me@mailhost.test".to_string()], &["mx.mailhost.test".to_string()], Some(1));
    assert_eq!(input.links.len(), 1, "javascript: link is dropped by the sanitiser");
    assert_eq!(input.dangerous_scheme_links, 1);
    assert_eq!(input.attachments.len(), 1);
    let r = analyze(&input);
    assert_eq!(r.level, Level::Warning);
    for code in ["AUTH_DMARC_FAIL", "SENDER_REPLYTO_MISMATCH", "SENDER_DISPLAY_NAME_BRAND", "LINK_IP_LITERAL", "LINK_DANGEROUS_SCHEME", "ATT_DOUBLE_EXT", "ATT_EXECUTABLE", "THREAD_FAKE_REPLY", "SENDER_FIRST_TIME"] {
        assert!(has(&r, code), "{code} missing in {:?}", codes(&r));
    }
}

#[test]
fn build_input_for_a_plain_benign_message() {
    let raw = b"From: Alice <alice@example.com>\r\nTo: me@mailhost.test\r\nSubject: Lunch\r\nMessage-ID: <1@example.com>\r\nAuthentication-Results: mx.mailhost.test; spf=pass; dkim=pass header.d=example.com; dmarc=pass\r\n\r\nSee you Friday! Menu: https://example.com/menu\r\n";
    let parsed = crate::mime::parse_message(raw).unwrap();
    let extra = crate::mime::parse_extra_headers(raw);
    let input = build_input(&parsed, &extra, &["me@mailhost.test".to_string()], &["mx.mailhost.test".to_string()], Some(4));
    assert_eq!(analyze(&input).level, Level::NoneFound);
}

#[test]
fn plain_text_message_links_are_checked_too() {
    let raw = b"From: Eve <eve@example.net>\r\nTo: me@mailhost.test\r\nSubject: hi\r\n\r\nPlease sign in at http://203.0.113.5/login now.\r\n";
    let parsed = crate::mime::parse_message(raw).unwrap();
    let extra = crate::mime::parse_extra_headers(raw);
    let input = build_input(&parsed, &extra, &[], &[], None);
    assert_eq!(input.links.len(), 1);
    assert!(has(&analyze(&input), "LINK_IP_LITERAL"));
}
