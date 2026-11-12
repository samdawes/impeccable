# Gemini image generation (this fork)

Upstream Impeccable generates comps and plates through a harness's own image tool or, as a fallback, the OpenAI Images API. This fork adds a second fallback: the Gemini API (Nano Banana models), selected by a `GEMINI_API_KEY`. Everything else in the workflow is unchanged: the same `impeccable generate-image` verb, the same comps, sidecars, embedded prompts and decision pages.

## What you need

1. **A Gemini API key** from Google AI Studio (aistudio.google.com, "Get API key").
2. **Billing on that key's project.** A Google AI Pro or Ultra subscription does not cover API calls by itself, but it includes monthly Google Cloud credits that do. Activate them through the Google Developer Program and link them to the billing account of the project that owns your key; AI Studio's "Use your Google AI plan credits" guide walks through it. After that, image calls draw down the credits before anything is charged.
3. **This fork's engine binary.** The skill's launcher downloads the upstream engine by default, which has no Gemini backend. Build this one:

   ```bash
   cargo build --release -p impeccable
   ```

## Point your agent at it

The key and the binary must be visible to the shell your coding agent runs commands in. For Claude Code, the most reliable place is the `env` block of your user settings, `~/.claude/settings.json` (never a file you commit):

```json
{
  "env": {
    "IMPECCABLE_BIN": "/absolute/path/to/impeccable/target/release/impeccable",
    "GEMINI_API_KEY": "your key"
  }
}
```

Exporting the same two variables in your shell profile works too, for agents launched from that shell. Restart the agent session afterwards.

`IMPECCABLE_BIN` makes the launcher run your build instead of downloading one, so this works with the Impeccable plugin as installed from upstream: the Gemini behavior lives in the engine, and the boot directive it prints tells the agent how to use it.

## Check it

From a project, run the engine's boot command and look for the directive:

```bash
"$IMPECCABLE_BIN" context | grep IMAGE_GEN_AVAILABLE
```

It should name `gemini-3-pro-image` and "billed to the user's Gemini API key". Then make one small test image, which spends one request:

```bash
"$IMPECCABLE_BIN" generate-image --prompt "A plain test card that reads OK" --out test.png --size 1024x1024
```

## Options

| Variable | Effect |
|---|---|
| `IMPECCABLE_GEMINI_IMAGE_MODEL` | Model to use. Default `gemini-3-pro-image` (Nano Banana Pro), the most legible with UI text, which comps depend on. `gemini-3.1-flash-image` (Nano Banana 2) is cheaper per image. If Google renames a model, a 404 from `generate-image` says so and this variable fixes it without a rebuild. |
| `IMPECCABLE_IMAGE_PROVIDER` | `gemini` or `openai`. Only needed if both keys are set; OpenAI wins otherwise. |

Gemini takes an aspect ratio and a size tier rather than exact pixels, so `--size 1536x1024` becomes 3:2 at 2K. The output line reports the real dimensions.

## Limits

- **No transparent backgrounds.** Gemini image models draw opaque pixels. `--background transparent` is refused rather than faked with a keyed-out ground (which leaves halos), so isolated cutout plates are reported as blocked and the agent hands the decision back to you. Full-frame plates and every comp work normally.
- **Upstream updates.** When you update the plugin from upstream and it pins a newer engine, merge upstream into this fork and rebuild, so your binary keeps up with the skill's commands.
