# Phoenix & LiveView Rules

## HEEx Template Syntax (Critical)
- **Attributes use `{}`**: `<div id={@id}>` — never `<%= %>` in attributes
- **Body values use `{}`**: `{@value}` — use `<%= %>` only for blocks (if/for/cond)
- **Class lists require `[]`**: `class={["base", @flag && "active"]}` — bare `{}` is invalid
- **No `else if`**: Use `cond` for multiple conditions
- **Comments**: `<%!-- comment --%>`
- **Literal curlies**: Use `phx-no-curly-interpolation` on parent tag

## Phoenix v1.8
- Wrap templates with `<Layouts.app flash={@flash}>` (already aliased)
- `current_scope` errors → move routes to proper `live_session`, pass to Layouts.app
- `<.flash_group>` only in layouts.ex
- Use `<.icon name="hero-x-mark">` for icons, `<.input>` for form fields

## LiveView
- Use `<.link navigate={}>` / `push_navigate`, not deprecated `live_redirect`
- Hooks with own DOM need `phx-update="ignore"`
- Avoid LiveComponents unless necessary
- No inline `<script>` tags — use assets/js/app.js

## Streams (Always use for collections)
```elixir
stream(socket, :items, items)           # append
stream(socket, :items, items, at: -1)   # prepend
stream(socket, :items, items, reset: true)  # filter/refresh
```
Template: `<div id="items" phx-update="stream">` with `:for={{id, item} <- @streams.items}`
- Streams aren't enumerable — refetch + reset to filter
- Empty states: `<div class="hidden only:block">Empty</div>` as sibling

## Forms
```elixir
# LiveView: always use to_form
assign(socket, form: to_form(changeset))
```
```heex
<%!-- Template: always @form, never @changeset --%>
<.form for={@form} id="my-form" phx-submit="save">
  <.input field={@form[:name]} type="text" />
</.form>
```
- Never `<.form let={f}>` or `<.form for={@changeset}>`

## Router
- Scope alias is auto-prefixed: `scope "/", AppWeb do` → `live "/users", UserLive` = `AppWeb.UserLive`

## Ecto
- Preload associations accessed in templates
- Use `Ecto.Changeset.get_field/2` to read changeset fields
- Don't cast programmatic fields (user_id) — set explicitly

## Testing
- Use `has_element?(view, "#my-id")`, not raw HTML matching
- Debug selectors: `LazyHTML.filter(LazyHTML.from_fragment(render(view)), "selector")`

