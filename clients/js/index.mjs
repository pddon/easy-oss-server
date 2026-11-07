import { createRequire } from 'module';

const require = createRequire(import.meta.url);
const mod = require('./index.js');

export const OSSClient = mod.OSSClient;
export const OSSError = mod.OSSError;
export const OSSNetworkError = mod.OSSNetworkError;
export const OSSTimeoutError = mod.OSSTimeoutError;
export const isOSSError = mod.isOSSError;
export const createClient = mod.createClient;

export default mod;
