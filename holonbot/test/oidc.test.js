
import { validateClaims } from '../lib/oidc.js';

describe('OIDC Validation', () => {
    test('should validate correct claims', () => {
        const claims = {
            repository: 'jolestar/holon',
            repository_owner: 'jolestar',
            actor: 'jolestar',
            ref: 'refs/heads/main'
        };

        const result = validateClaims(claims);
        expect(result).toEqual({
            repository: 'jolestar/holon',
            owner: 'jolestar',
            actor: 'jolestar',
            ref: 'refs/heads/main'
        });
    });

    test('should throw error if repository is missing', () => {
        const claims = {
            actor: 'jolestar'
        };
        expect(() => validateClaims(claims)).toThrow('Missing repository information');
    });

    test('should throw error if owner is missing', () => {
        const claims = {
            repository: 'jolestar/holon'
        };
        expect(() => validateClaims(claims)).toThrow('Missing repository information');
    });
});
