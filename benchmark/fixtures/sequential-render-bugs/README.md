# Sequential Render Bugs Fixture

This fixture contains two independent formatting issues in a tiny render flow:

- `src/name.js` lowercases the display name
- `src/role.js` uppercases the role label

The benchmark is designed so an agent may fix one issue, rerun verification,
and then continue until the full rendering contract passes.
