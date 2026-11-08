<script lang="ts">
  import { app } from "../lib/state.svelte";
  import Modal from "./Modal.svelte";
  import AboutContent from "./AboutContent.svelte";
</script>

<Modal title="About MiraMail" onclose={() => (app.showAbout = false)} width={440}>
  <AboutContent />
</Modal>
