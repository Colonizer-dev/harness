**The out-of-quota card can switch every role at once, and remember a non-Claude fallback.** The
card's Switch (and Settings → Providers) now offers **every role using this provider**: every model
role in the install's agent settings (orchestrator, subagent, background, summary, small- and
large-task models) and every org override that points at the exhausted provider move to the chosen
model, and the card's colonies restart on it. Each role is checked first, so a model one of them
cannot take (Claude as the summary model with no Anthropic API key, say) changes nothing; afterwards
the cockpit lists each setting as "was X → now Y". **Remember as fallback** and the provider form's
fallback picker now also take a model on another provider that speaks the same wire (anthropic to
anthropic, openai to openai): when the provider answers quota exhausted, the Mothership retries the
request there itself. A cross-wire fallback is refused with the reason. ([#767])
