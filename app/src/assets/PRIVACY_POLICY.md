# MiraMail Privacy Policy

**Effective date:** 2026-11-06
**Publisher:** MIRASTACK LABS PRIVATE LIMITED ("we", "us")
**Contact:** hello@mirastacklabs.ai

MiraMail is a free email app for macOS. This policy says in plain language what the app does with your information. Short version: the MiraMail app sends nothing to MIRASTACK LABS. MiraMail has no telemetry, no analytics, no advertising and no account with us. MIRASTACK LABS also runs Endless Tokens, a separate optional AI service with its own account and policy; MiraMail contacts it only if you choose it, enter your key, and use MIRA or Check tone.

## What we collect
The MiraMail app collects nothing and sends nothing to us. We do not receive your email address, password, mail, contacts, calendar data, usage statistics, crash reports, device identifiers or location through the app, and the app gives us no way to see any of it. (The only MIRASTACK LABS service involved is the separate, optional Endless Tokens service described below, which you must choose and sign up for yourself.)

## Where your information goes
- **Your mail provider.** When you add an account, MiraMail connects directly from your Mac to the IMAP and SMTP servers of your mail provider (for example Zoho, Fastmail, iCloud or your own domain) to read and send your mail. These connections use TLS wherever the server supports it. Your provider's own privacy policy applies to that data.
- **Account setup lookups.** To find your mail servers from your email address, the app makes ordinary DNS lookups and uses a provider list bundled inside the app. If those do not settle it, it also requests the standard mail-settings (autoconfig) file over HTTPS from your email address's own domain: `https://autoconfig.<domain>/mail/config-v1.1.xml?emailaddress=<your address>` and `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`. Your full email address is part of the first request, so the web host of that domain receives it (and your IP address). This goes to your domain's host, never to us or to a lookup service of ours.
- **Your password** is stored only in the macOS Keychain on your Mac. It is sent only to your own mail server to log in. It never leaves your Mac to us.

## Optional features (all off until you set them up)
- **Calendar (CalDAV).** If you add a calendar, MiraMail connects only to the CalDAV server address you enter, to read and update your events. Replies to invitations and invitations you create are sent as email through your own mail server.
- **Zoom.** If you choose to connect a Zoom account, MiraMail contacts Zoom (zoom.us and api.zoom.us) to sign you in with OAuth and to create meetings you ask for. Nothing is sent to Zoom before you connect. Zoom's privacy policy applies to that data. Sign-in tokens are kept in the macOS Keychain.
- **MIRA AI assistant (your own AI endpoint).** MIRA is off by default. In Settings > AI you enter the web address (and, if needed, key) of an AI service you choose. MiraMail contacts that address only from your Mac, and only after you press Test or Save in the settings form (or enter a key there, which fetches the model list from that service with one GET /models request and nothing else). The settings form offers presets (OpenAI, Anthropic, Endless Tokens and others) that only fill in an address; none is pre-selected, and nothing is contacted unless you press Test or Save or enter a key. Parts of your mail are sent to your chosen AI service only inside a conversation you started yourself, after you accept a consent dialog, and only from the folders you ticked there or later allow with Edit permissions (plus any folder MIRA creates for you in that conversation after you approve it). Calendar details (event titles, times, locations, attendees, calendar names) are sent only if you switch on "Let MIRA see my calendar" for that conversation, and only for the calendars you tick. MIRA can also read the names and plain-language descriptions of your existing mail rules for the accounts you ticked (never message content) so it can explain them or avoid duplicates; they are sent to your chosen service only inside a conversation you started with "Assistant may suggest changes" on. MIRA can propose changes (such as moving a message or adding a calendar event); these happen only after you click Approve. MIRA never sends mail by itself and never permanently deletes mail. Separately, if you press "Check tone" in the Compose window and confirm the notice, the text of that one draft is sent once to the same service (see "Check tone" below). Your chosen AI service's own terms and privacy policy apply to what it receives. If you use an address that is not encrypted (plain http to a non-local host), MiraMail warns you that your key and mail content would travel unencrypted. Local rules and the template auto-reply never contact any AI service.
- **Unsubscribe.** If you press Unsubscribe on a mailing list message and confirm, MiraMail makes one request to the unsubscribe address the sender put in the message (or sends an unsubscribe email through your own mail server). Nothing else is sent.
- **Remote images** in emails are blocked by default. If you choose to load them, your Mac fetches them from the sender's server, which can tell the sender you opened the message.

## Optional AI assistant (MIRA)
- Nothing is sent to any AI service unless you set one up yourself: you must enter an endpoint (and a key, if that provider needs one) in Settings > AI and then start a MIRA conversation yourself.
- When you start a conversation, a dialog names the provider (its label and host) that will receive what you type and, if you allow it, the mail and calendar details MIRA reads. You choose the folders and calendars. Nothing is sent before you press Start. You can later change which folders and calendars an existing conversation may see from its Edit permissions dialog (it names the same provider and host); nothing is sent by opening or cancelling it, and a change takes effect only after you press Save permissions. If you later remove a folder with Edit permissions, mail MIRA read earlier from that folder is no longer sent with later messages. Your own messages and MIRA's earlier replies (which may summarise mail) stay in the chat and are still sent; start a new chat to drop them. When tools are on, each request also includes the current date, time zone and your work hours so MIRA can answer time questions.
- Your content goes directly from your Mac to the provider you chose, under your own account with that provider and under that provider's own policy. MiraMail has no server in between and never sees it.
- **Check tone (optional, one press at a time).** The Compose window shows a small tone hint that is worked out on your Mac while you type; it sends nothing, and you can turn it off in Preferences > Composing. If you press "Check tone", a notice names the provider (its label and host) and says that this draft's own text (no quoted message, no signature, no recipients) will be sent there once to check its tone, that email addresses typed in the text are included unless address masking is on, that MiraMail does not store it and the provider's own policy applies, and that nothing is sent until you press Send in that notice. Exactly one request is made, with no mailbox or calendar content and no conversation (at most 6,000 characters). If the shorten-links or mask-addresses settings in Settings > AI are on, links and addresses in the draft are replaced by numbered placeholders ([link-1], [person-1]) before sending, and MiraMail puts your real links and addresses back into a suggestion before you apply it; a very long link, or more than 50 of them, cannot be restored (a suggestion with a placeholder it cannot match is not applied, but you can still Copy it). Check tone requests are not shown in the "Sent to model" log and are not counted in the daily token limits. The reply is shown as plain text and changes your draft only if you press Apply.
- Provider policies: [Endless Tokens](https://portal.endlesstokens.ai/legal/privacy), [OpenAI](https://openai.com/policies/privacy-policy), [Anthropic](https://www.anthropic.com/legal/privacy). If you use another or a local provider, read its policy.
- Endless Tokens is a separate, optional service, operated by MIRASTACK LABS PRIVATE LIMITED, that you must choose and sign up for yourself with your own key. It is operated separately from this app. Its published policy (linked above) says it keeps request metadata (time, model, token counts, IP address) for 3 days and does not store prompts or completions; that policy governs what it does with requests.

## Data stored on your Mac
MiraMail keeps a local cache (message headers and bodies, search index, settings, rules, drafts, the outbox, colour labels, calendar data, AI conversation history) in its own storage on your Mac. Passwords and tokens are in the Keychain. Removing an account in MiraMail removes its local data. You can also delete the app's data folder yourself. We cannot access any of it.

## Sharing and selling
We do not sell, rent or share your data, because the app does not send it to us. We do not use it for advertising or tracking.

## Children
MiraMail is a general-purpose email app and is not directed at children. We collect no personal information from anyone.

## Security
Passwords live in the macOS Keychain, connections use TLS wherever the server supports it (MiraMail warns before unencrypted AI connections), and HTML email is displayed in a sandbox with scripts disabled. No system is perfectly secure; keep your Mac updated and use app-specific passwords where your provider offers them.

## Your rights
Because the app sends us no data about you, there is nothing in the app for us to access, correct or delete. For data held by Endless Tokens, see its own policy and contact its operator. For data held by your mail provider, calendar server, Zoom or AI service, contact them.

## Changes
If this policy changes we will update the effective date and, for material changes, note it in the app's release notes.

## Contact
MIRASTACK LABS PRIVATE LIMITED, 4th floor, Survey No. 322(P), Myscape Weave, Financial District, Hyderabad, Telangana 500089, India. Email: hello@mirastacklabs.ai

---

