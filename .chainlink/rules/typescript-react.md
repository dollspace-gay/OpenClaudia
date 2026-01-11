### TypeScript/React Best Practices

#### Component Structure
- Use functional components with hooks
- Keep components small and focused (< 200 lines)
- Extract custom hooks for reusable logic
- Use TypeScript interfaces for props

```typescript
// GOOD: Typed props with clear interface
interface UserCardProps {
    user: User;
    onSelect: (id: string) => void;
}

const UserCard: React.FC<UserCardProps> = ({ user, onSelect }) => {
    return (
        <div onClick={() => onSelect(user.id)}>
            {user.name}
        </div>
    );
};
```

#### State Management
- Use `useState` for local state
- Use `useReducer` for complex state logic
- Lift state up only when needed
- Consider context for deeply nested prop drilling

#### Performance
- Use `React.memo` for expensive pure components
- Use `useMemo` and `useCallback` appropriately (not everywhere)
- Avoid inline object/function creation in render when passed as props

#### Security
- Never use `dangerouslySetInnerHTML` with user input
- Sanitize URLs before using in `href` or `src`
- Validate props at component boundaries
