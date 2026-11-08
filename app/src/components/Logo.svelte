<script lang="ts">
  let { size = 18, word = true }: { size?: number; word?: boolean } = $props();
</script>

<span class="logo" style="--s:{size}px">
  <!-- MiraMail mark: original MIRASTACK LABS envelope drawing, teal -->
  <svg viewBox="0 0 24 18" height={size * 1.1} aria-hidden="true">
    <rect x="0" y="0" width="24" height="18" rx="4" fill="#2bd4bf" />
    <path d="M3.5 4.5 L12 11 L20.5 4.5" fill="none" stroke="#0a0a0a" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" />
  </svg>
  {#if word}<span class="word">MIRAMAIL</span>{/if}
</span>

<style>
  .logo { display: inline-flex; align-items: center; gap: 10px; }
  svg { width: auto; display: block; }
  .word { font: 700 calc(var(--s) * .78) var(--font-mono); letter-spacing: .22em; color: var(--ink); }
</style>
