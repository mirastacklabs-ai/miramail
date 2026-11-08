<script lang="ts">
  import { app } from "../lib/state.svelte";
  import { openExternal } from "../lib/api";
  import notices from "../../../THIRD_PARTY_NOTICES.md?raw";
  import fullLicences from "../../../THIRD_PARTY_LICENSES.txt?raw";
  import licence from "../../../LICENSE?raw";
  import privacy from "../assets/PRIVACY_POLICY.md?raw";

  const version = __APP_VERSION__;

  async function site() {
    const url = "https://www.mirastacklabs.ai";
    const ok = await app.ask({ title: "Open website?", body: "This opens the address below in your default browser.", detail: url, ok: "Open" });
    if (!ok) return;
    try { await openExternal(url); } catch (e) { app.fail(e); }
  }
</script>

  <div class="about">
    <svg viewBox="100 100 824 824" width="96" height="96" role="img" aria-label="MiraMail icon">
      <defs>
        <linearGradient id="ab-bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#16252a" /><stop offset="1" stop-color="#070b0d" /></linearGradient>
        <linearGradient id="ab-tl" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#5eeadb" /><stop offset="1" stop-color="#2bd4bf" /></linearGradient>
        <linearGradient id="ab-td" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#1fd1ba" /><stop offset="1" stop-color="#00a38f" /></linearGradient>
      </defs>
      <rect x="100" y="100" width="824" height="824" rx="186" fill="url(#ab-bg)" stroke="#ffffff" stroke-opacity=".16" stroke-width="3" />
      <g>
        <rect x="232" y="296" width="560" height="368" rx="56" fill="url(#ab-tl)" />
        <path d="M232 540v68a56 56 0 0 0 56 56h448a56 56 0 0 0 56-56v-68z" fill="url(#ab-td)" />
        <path d="M290 366L512 540 734 366" fill="none" stroke="#0a1114" stroke-width="56" stroke-linecap="round" stroke-linejoin="round" />
        <rect x="312" y="722" width="400" height="38" rx="19" fill="url(#ab-tl)" />
        <rect x="392" y="794" width="240" height="38" rx="19" fill="url(#ab-td)" />
      </g>
    </svg>
    <h3>MIRAMAIL</h3>
    <p class="mono ver">Version {version}</p>
    <p>A free, ad-free, AI-ready mail client for macOS. No telemetry. No ads. Your passwords stay in the macOS Keychain.</p>
    <p class="attr">
      <strong>© MIRASTACK LABS PRIVATE LIMITED</strong><br />
      The colours and visual identity of MiraMail are taken from MIRASTACK LABS.
    </p>
    <p class="mono small">Free to use. All rights reserved.</p>
    <button class="btn ghost sm" onclick={site}>mirastacklabs.ai</button>
    <details class="doc">
      <summary>Privacy policy</summary>
      <!-- svelte-ignore a11y_no_noninteractive_tabindex --><pre tabindex="0">{privacy}</pre>
    </details>
    <details class="doc">
      <summary>Open-source licences / Third-party notices</summary>
      <!-- svelte-ignore a11y_no_noninteractive_tabindex --><pre tabindex="0">{notices}</pre>
    </details>
    <details class="doc">
      <summary>Full licence texts</summary>
      <!-- svelte-ignore a11y_no_noninteractive_tabindex --><pre tabindex="0">{fullLicences}</pre>
    </details>
    <details class="doc">
      <summary>MiraMail licence</summary>
      <!-- svelte-ignore a11y_no_noninteractive_tabindex --><pre tabindex="0">{licence}</pre>
    </details>
  </div>


<style>
  .about { display: grid; justify-items: center; gap: 10px; text-align: center; }
  h3 { margin: 6px 0 0; letter-spacing: .22em; }
  p { margin: 0; color: var(--text); font-size: 0.9333rem; }
  .ver, .small { color: var(--muted); font-size: 0.8rem; }
  .doc { width: 100%; text-align: left; font-size: 0.8667rem; }
  .doc summary { cursor: pointer; color: var(--muted); padding: 4px 0; }
  .doc pre { max-height: 220px; overflow: auto; margin: 6px 0 0; padding: 8px; white-space: pre-wrap; overflow-wrap: anywhere; font-size: 0.7333rem; line-height: 1.45; border: 1px solid var(--border, #ffffff22); border-radius: 6px; user-select: text; }
  .attr { margin-top: 6px; line-height: 1.6; }
</style>
