//! Small bundled lists for the safety checks. All heuristics: they trade completeness for explainability.

use super::urls::{reg_domain, reg_label};

/// Well-known brands that phishing imitates: (brand label, extra registrable domains that are legitimately theirs).
/// Brand labels shorter than 5 characters are left out on purpose (too many innocent look-alikes).
pub const BRANDS: &[(&str, &[&str])] = &[
    ("paypal", &[]),
    ("apple", &["icloud.com", "me.com", "cdn-apple.com", "mzstatic.com"]),
    ("google", &["googleapis.com", "googleusercontent.com", "gstatic.com", "googlemail.com", "youtube.com", "gmail.com", "goo.gl", "withgoogle.com"]),
    ("amazon", &["amazonaws.com", "amazonses.com", "a2z.com", "amzn.to", "amzn.com"]),
    ("microsoft", &["microsoftonline.com", "office.com", "office365.com", "live.com", "outlook.com", "sharepoint.com", "windows.net", "azure.com", "msn.com", "bing.com", "skype.com", "windows.com", "xbox.com", "hotmail.com"]),
    ("netflix", &["nflxso.net", "nflximg.net"]),
    ("facebook", &["facebookmail.com", "fb.com", "meta.com", "instagram.com", "whatsapp.com", "fbcdn.net"]),
    ("instagram", &["facebookmail.com", "cdninstagram.com", "instagram.com"]),
    ("linkedin", &["licdn.com"]),
    ("docusign", &["docusign.net"]),
    ("dropbox", &["dropboxmail.com", "dropboxusercontent.com"]),
    ("coinbase", &[]),
    ("binance", &[]),
    ("whatsapp", &["whatsapp.net", "facebook.com"]),
    ("telegram", &["t.me", "telegram.me"]),
    ("twitter", &["x.com", "t.co", "twimg.com"]),
    ("wellsfargo", &[]),
    ("bankofamerica", &["bofa.com"]),
    ("citibank", &["citi.com"]),
    ("barclays", &[]),
    ("santander", &[]),
    ("revolut", &[]),
    ("spotify", &["scdn.co"]),
    ("airbnb", &[]),
    ("walmart", &[]),
    ("outlook", &["live.com", "office365.com", "microsoft.com", "office.com", "hotmail.com"]),
    ("office365", &["office.com", "microsoft.com", "microsoftonline.com"]),
    ("icloud", &["apple.com", "me.com"]),
    ("github", &["githubusercontent.com", "github.io"]),
    ("stripe", &["stripe.network"]),
];

/// Names that are one edit away from a brand but are real, unrelated services.
pub const NEAR_BRAND_OK: &[&str] = &["paypay"];

/// Is `reg` (a registrable domain) one of the brand's own domains?
pub fn is_official(brand: &str, reg: &str) -> bool {
    let label = reg.split('.').next().unwrap_or("");
    if label == brand {
        return true;
    }
    BRANDS.iter().any(|(b, extra)| *b == brand && extra.contains(&reg))
}

pub fn brand_label(label: &str) -> Option<&'static str> {
    BRANDS.iter().map(|(b, _)| *b).find(|b| *b == label)
}

/// Hosts of mailing services, click trackers and link-protection rewriters. A link through one of these is
/// normal in legitimate mail, so it never counts as a text/address mismatch on its own.
const ESP_SUFFIXES: &[&str] = &[
    "list-manage.com", "mailchimp.com", "mcsv.net", "rsgsv.net", "sendgrid.net", "sendgrid.com", "mandrillapp.com", "mailgun.org",
    "mailgun.com", "amazonses.com", "awstrack.me", "sparkpostmail.com", "customeriomail.com", "customer.io", "klclick.com",
    "klaviyomail.com", "klaviyo.com", "hubspotlinks.com", "hubspotemail.net", "hs-analytics.net", "hsforms.com", "hubspot.com",
    "sendinblue.com", "sibautomation.com", "brevo.com", "sendibt3.com", "sendibm1.com", "constantcontact.com", "rs6.net",
    "campaign-archive.com", "createsend.com", "cmail19.com", "cmail20.com", "exacttarget.com", "salesforce.com", "eloqua.com",
    "marketo.com", "mktdns.com", "mkto-sj.com", "pardot.com", "intercom-mail.com", "intercom.io", "mailerlite.com", "mlsend.com",
    "convertkit-mail.com", "convertkit-mail2.com", "ck.page", "substack.com", "beehiiv.com", "mailjet.com", "mjt.lu", "postmarkapp.com",
    "pstmrk.it", "getresponse.com", "activecampaign.com", "acemlnc.com", "drip.com", "omnisend.com", "omnisrv.com", "braze.com",
    "sailthru.com", "emltrk.com", "doubleclick.net", "googleadservices.com", "t.co",
    // Link-protection rewriters used by organisations.
    "safelinks.protection.outlook.com", "urldefense.com", "urldefense.proofpoint.com", "mimecast.com", "mimecastprotect.com",
    "cudasvc.com", "secure-web.cisco.com", "clicktime.symantec.com", "emailprotection.link",
    // Helpdesk / reply-routing services that legitimately put their own address in Reply-To.
    "zendesk.com", "freshdesk.com", "helpscout.net", "helpscoutapp.com", "desk.com", "uservoice.com", "atlassian.net", "mailgun.net",
];

pub fn is_esp_host(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_lowercase();
    ESP_SUFFIXES.iter().any(|s| h == *s || h.strip_suffix(s).is_some_and(|p| p.ends_with('.')))
}

const SHORTENERS: &[&str] = &[
    "bit.ly", "tinyurl.com", "goo.gl", "ow.ly", "is.gd", "buff.ly", "rebrand.ly", "cutt.ly", "shorturl.at", "tiny.cc", "rb.gy", "t.ly",
    "lnkd.in", "bl.ink", "s.id", "v.gd", "soo.gd", "bitly.com", "tr.im", "cli.re", "shorte.st", "adf.ly",
];

pub fn is_shortener(host: &str) -> bool {
    let h = host.to_lowercase();
    SHORTENERS.iter().any(|s| h == *s)
}

const SUSPICIOUS_TLDS: &[&str] = &["xyz", "top", "click", "zip", "mov", "work", "support", "country", "gq", "tk", "ml", "cf", "ga", "loan", "rest", "buzz", "icu", "cam", "monster", "cyou", "sbs", "cfd", "quest"];

pub fn suspicious_tld(host: &str) -> Option<&'static str> {
    let tld = host.rsplit('.').next()?;
    SUSPICIOUS_TLDS.iter().find(|t| **t == tld).copied()
}

/// Common TLDs: the text in a link must end in one of these to be read as "a web address shown to the user".
const COMMON_TLDS: &[&str] = &[
    "com", "net", "org", "io", "co", "uk", "de", "fr", "es", "it", "nl", "be", "ch", "at", "se", "no", "dk", "fi", "pl", "cz", "ru", "ua",
    "us", "ca", "au", "nz", "jp", "cn", "in", "br", "mx", "ar", "za", "info", "biz", "me", "tv", "app", "dev", "ai", "edu", "gov", "eu", "ie",
    "pt", "gr", "tr", "kr", "sg", "hk", "tw", "il", "ae", "xyz", "online", "site", "shop", "store", "tech", "cloud", "page", "link", "email",
];

pub fn common_tld(tld: &str) -> bool {
    COMMON_TLDS.contains(&tld)
}

const FREEMAIL: &[&str] = &[
    "gmail.com", "googlemail.com", "yahoo.com", "outlook.com", "hotmail.com", "live.com", "msn.com", "aol.com", "icloud.com", "me.com",
    "proton.me", "protonmail.com", "gmx.com", "gmx.net", "mail.com", "yandex.com", "yandex.ru", "zoho.com", "qq.com", "163.com", "126.com",
];

pub fn is_freemail(host: &str) -> bool {
    let reg = reg_domain(host);
    FREEMAIL.contains(&reg.as_str()) || matches!(reg_label(host).as_str(), "yahoo" | "hotmail" | "outlook" | "gmx")
}
