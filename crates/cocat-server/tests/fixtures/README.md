Test-only data.

`oidc_test_key.pem` is an RSA 2048 key generated with `openssl genrsa` for the fake OpenID
Connect provider in `tests/oidc_support`. Its public half is served as the provider's JWKS, so
the tests can sign ID tokens without a network key exchange. It protects nothing: never use it
anywhere but these tests.
