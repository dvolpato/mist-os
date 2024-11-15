# Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

FROM alpine:3 AS build 
RUN apk update && \
    apk add --no-cache gcc musl-dev
COPY cowsay.c .
RUN gcc -o ./cowsay ./cowsay.c

FROM alpine:3 AS final
COPY --from=build cowsay /usr/local/bin/cowsay
CMD ["cowsay", "Hello from MistOS!"]