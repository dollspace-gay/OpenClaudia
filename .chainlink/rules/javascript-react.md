### JavaScript/React Best Practices

#### Component Structure
- Use functional components with hooks
- Keep components small and focused (< 200 lines)
- Extract custom hooks for reusable logic
- Use PropTypes for runtime type checking

```javascript
// GOOD: Clear component with PropTypes
import PropTypes from 'prop-types';

const UserCard = ({ user, onSelect }) => {
    return (
        <div onClick={() => onSelect(user.id)}>
            {user.name}
        </div>
    );
};

UserCard.propTypes = {
    user: PropTypes.shape({
        id: PropTypes.string.isRequired,
        name: PropTypes.string.isRequired,
    }).isRequired,
    onSelect: PropTypes.func.isRequired,
};
```

#### State Management
- Use `useState` for local state
- Use `useReducer` for complex state logic
- Lift state up only when needed
- Consider context for deeply nested prop drilling

#### Performance
- Use `React.memo` for expensive pure components
- Use `useMemo` and `useCallback` appropriately
- Avoid inline object/function creation in render

#### Security
- Never use `dangerouslySetInnerHTML` with user input
- Sanitize URLs before using in `href` or `src`
- Validate props at component boundaries
