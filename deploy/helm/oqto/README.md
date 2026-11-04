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

## Storage and scaling

The default 10Gi ReadWriteOnce PVC stores Oqto and EAVS state under `/data`.
Oqto's all-in-one deployment uses local SQLite state, so the chart requires one
replica and uses a `Recreate` rollout strategy. Set `persistence.enabled=false`
only for disposable testing.

The image entrypoint currently starts as root to initialize ownership and
services; leave `containerSecurityContext` unset unless your image has been
adapted for a non-root runtime. For private registries, configure
`imagePullSecrets`.
