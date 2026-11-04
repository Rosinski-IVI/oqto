# Oqto Helm Chart

This chart deploys Oqto's all-in-one container behind a ClusterIP Service. It
does not create an Ingress, Gateway, or external load balancer.

## Install

Review `values.yaml`, then install into a dedicated namespace:

```sh
helm upgrade --install oqto ./deploy/helm/oqto \
  --namespace oqto --create-namespace
```

Access it locally with port-forwarding:

```sh
kubectl -n oqto port-forward service/oqto 8080:8080
```

Then open `http://localhost:8080`.

## Secrets

The chart creates a Kubernetes Secret named `<release>-oqto-env` by default.
Set provider keys and an admin password in a private values file under
`secret.data`; do not commit that file. For example:

```yaml
secret:
  data:
    ADMIN_PASSWORD: "replace-me"
    FHGENIE_API_KEY: "replace-me"
```

For externally managed Secrets, set `secret.existingSecret` to its name. The
Secret's keys are injected as environment variables. Supported provider keys
include `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`,
`OPENROUTER_API_KEY`, `AZURE_API_KEY`, `DEEPSEEK_API_KEY`,
`MISTRAL_API_KEY`, and `FHGENIE_API_KEY`. `ADMIN_PASSWORD` and `JWT_SECRET`
may also be supplied this way.

## OpenAI-compatible providers

`openaiCompatibleProviders` is a list of EAVS `openai-compatible` providers.
Each entry has a unique `name`, `baseUrl`, optional `apiKeyEnv`, and a `models`
list. Provider entries can also set `supportsDeveloperRole`. Model entries
support `id`, `name`, `reasoning`, `input`, `context_window`, `max_tokens`,
`cost`, `compat`, and `thinking_level_map`.

The default `fhgenie` provider includes GLM 5.3 and DeepSeek V4.1 Flash. To add
another provider, include it in the list and provide its key through the chart
Secret or `secret.existingSecret`:

```yaml
openaiCompatibleProviders:
  - name: groq
    baseUrl: https://api.groq.com/openai/v1
    apiKeyEnv: GROQ_API_KEY
    models:
      - id: llama-3.3-70b-versatile
        name: Llama 3.3 70B
        reasoning: false
        input: [text]

secret:
  data:
    GROQ_API_KEY: "replace-me"
```

When supplying this list in a values override, include every provider you want
enabled; Helm replaces arrays rather than appending to them. `apiKeyEnv` is
optional for unauthenticated endpoints. `thinking_level_map` maps Pi effort
levels to provider values; use `null` for unsupported levels. The chart writes
provider definitions to a ConfigMap, and the container generates EAVS and Pi
configuration from it at startup. Provider names must not collide with the
built-in `anthropic`, `openai`, `google`, `openrouter`, or `local` providers.

## Storage and scaling

The default 10Gi ReadWriteOnce PVC stores Oqto and EAVS state under `/data`.
Oqto's all-in-one deployment uses local SQLite state, so the chart requires one
replica and uses a `Recreate` rollout strategy. Set `persistence.enabled=false`
only for disposable testing.

The image entrypoint currently starts as root to initialize ownership and
services; leave `containerSecurityContext` unset unless your image has been
adapted for a non-root runtime. For private registries, configure
`imagePullSecrets`.
