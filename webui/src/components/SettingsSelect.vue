<template>
  <v-row justify-md="center" justify-sm="start">
    <v-col cols="5" class="text-left" align-self="center">
      <span class="">{{ label }}</span>
    </v-col>
    <v-col cols="7">
      <v-select density="compact" variant="solo"
        :items="items"
        :model-value="modelValue"
        @update:model-value="$emit('update:modelValue', $event)"
        hide-details
      />
    </v-col>
  </v-row>
</template>

<script lang="ts">
import {defineComponent} from 'vue'

export default defineComponent({
  name: 'SettingsSelect',
  props: {
    items: null as any,
    label: {
      type: String,
    },
    modelValue: {
      type: [String, Number],
    },
    display: {
      type: String,
    },
  },
})
</script>

<style>
  .v-input__details, div.v-input__control {
    min-height: 0 !important;
  }
  .v-input__details {
    display: none !important;
  }

</style>
