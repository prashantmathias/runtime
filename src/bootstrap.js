// src/bootstrap.js
const {
    op_custom_print,
    op_sleep,
    op_fs_read_file_sync,
    op_fs_write_file_sync,
    op_fs_read_file_async,
    op_fs_write_file_async
} = Deno.core.ops;

globalThis.console = {
    log(...args) {
        const message = args.map(arg => {
            if (typeof arg === 'object' && arg !== null) {
                return JSON.stringify(arg);
            }
            return String(arg);
        }).join(' ');
        op_custom_print(message, false);
    },
    error(...args) {
        const message = args.map(arg => {
            if (typeof arg === 'object' && arg !== null) {
                return JSON.stringify(arg);
            }
            return String(arg);
        }).join(' ');
        op_custom_print(message, true);
    }
};

globalThis.setTimeout = (callback, delay, ...args) => {
    op_sleep(delay || 0).then(() => callback(...args));
};

globalThis.fs = {
    readFileSync(path) {
        return op_fs_read_file_sync(path);
    },
    writeFileSync(path, contents) {
        op_fs_write_file_sync(path, contents);
    },
    readFile(path) {
        return op_fs_read_file_async(path);
    },
    writeFile(path, contents) {
        return op_fs_write_file_async(path, contents);
    }
};
