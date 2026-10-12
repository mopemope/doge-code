# Timeout configuration service
A small internal worker uses settings.parse_timeout to normalize a JSON timeout field.
Contract: positive finite integer/float values and whitespace-padded decimal numeric strings are accepted as float seconds. Boolean, None, objects, empty/non-numeric strings, zero/negative, NaN, infinity and numeric overflow must raise ValueError consistently. Do not change the public signature or add dependencies.
Run: python3 -m unittest discover -v
