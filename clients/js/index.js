'use strict';

const client = require('./lib/client');

module.exports = {
  OSSClient: client.OSSClient,
  OSSError: client.OSSError,
  OSSNetworkError: client.OSSNetworkError,
  OSSTimeoutError: client.OSSTimeoutError,
  isOSSError: require('./lib/errors').isOSSError,
  createClient(options) {
    return new client.OSSClient(options);
  },
};
