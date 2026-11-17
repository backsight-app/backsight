<script lang="ts">
  // Shown when the settings file couldn't be read: the pages then show defaults, not the user's choices,
  // and nothing is saved or started until the file is fixed.
  import { app } from "./settings.svelte";
</script>

{#if app.status === "error" && !app.loaded}
  <div class="problem" role="alert">
    <strong>Backsight couldn't read your settings, so it hasn't started recording.</strong>
    <p>
      What you see here are the standard settings, not yours. Nothing you change will be saved, so your file stays as it is.
    </p>
    {#if app.error?.includes("newer Backsight")}
      <p>A newer Backsight saved them. Install the newest Backsight to use them; don't delete the file.</p>
    {:else}
      <p>Fix or remove the file, then restart Backsight:</p>
      <code>%APPDATA%\Backsight\config\settings.toml</code>
    {/if}
    {#if app.error}<p class="detail">{app.error}</p>{/if}
  </div>
{/if}

<style>
  .problem {
    border: 1px solid #c0392b;
    background: rgba(192, 57, 43, 0.12);
    border-radius: 8px;
    padding: 12px 16px;
    margin-bottom: 12px;
  }
  p { margin: 6px 0 0; }
  .detail { opacity: 0.75; font-size: 0.9em; }
</style>
