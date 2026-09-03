# Build the G1.1 I/O fault-injection shim.
UNAME := $(shell uname -s)

ifeq ($(UNAME),Darwin)
TARGET := libiofault.dylib
LDFLAGS := -dynamiclib
else
TARGET := libiofault.so
LDFLAGS := -shared
endif

CFLAGS := -O2 -Wall -Wextra -fPIC

all: $(TARGET)

$(TARGET): iofault.c
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $< -ldl

clean:
	rm -f libiofault.dylib libiofault.so
