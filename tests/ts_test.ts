interface User {
    name: string;
    id: number;
}

function printUser(user: User): void {
    console.log(`[TS Test] User name is: ${user.name}, ID is: ${user.id}`);
}

const testUser: User = { name: "Alice", id: 42 };
printUser(testUser);

// Generic function
function identity<T>(arg: T): T {
    return arg;
}

const value: string = identity<string>("TypeScript works!");
console.log(`[TS Test] Generic identity returned: "${value}"`);
