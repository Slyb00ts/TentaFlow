# Template Addon

Demo addon — a template for creating new TentaFlow addons. Use this as a reference for the SKILL.md format.

## Tools

### template.hello
Returns a greeting message. This is a demo tool showing how tool calling works.

When to use:
- User wants to test addon functionality
- User asks for a demo or greeting
- Testing tool calling pipeline

TOON examples:
- `@template.hello`
- `@template.hello|name=Jan`
- `#MISSING@template.hello|name=?`

Parameters:
- **name** — name of the person to greet (default: "World")

## Scenarios

### Basic greeting
```toon
@template.hello|name=TentaFlow
```

## Notes
- This addon is a template — use it as a starting point for new addons
- The SKILL.md file should be clean markdown without YAML frontmatter
- All machine-readable data (keywords, disambiguation, category) belongs in manifest.toml
- Tool descriptions and TOON examples help the LLM understand when and how to use each tool
